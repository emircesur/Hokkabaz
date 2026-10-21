//! `hokkabazd`: headless Hokkabaz instance serving the agent API (`/api/v1`).

mod secrets;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use hk_llm::deepseek::DeepSeek;
use hk_llm::{CancellationToken, ChatRequest, EventStream, LlmError, ModelInfo, Provider};
use hk_server::{AppState, Auth, LocalConfig, LocalRuntime, Scope};
use hk_store::secrets::KeyringSecretStore;
use hk_store::{SecretStore, Store};

use crate::secrets::FileSecretStore;

const DEEPSEEK_ENV: &str = "DEEPSEEK_API_KEY";
const DEEPSEEK_SECRET: &str = "deepseek";

#[derive(Debug, Parser)]
#[command(name = "hokkabazd", version, about = "Hokkabaz headless instance")]
struct Cli {
    /// Data directory (sessions, db, secrets). Default: ~/.hokkabaz.
    #[arg(long, global = true, env = "HOKKABAZ_HOME")]
    data_dir: Option<PathBuf>,
    /// Listen address. Loopback by default; bind wider only behind TLS termination.
    #[arg(long, env = "HOKKABAZ_LISTEN", default_value = "127.0.0.1:7878")]
    listen: SocketAddr,
    /// Model for sessions created without one.
    #[arg(long, default_value = "deepseek-chat")]
    model: String,
    /// Override the DeepSeek API endpoint (gateway, mock server).
    #[arg(long, env = "DEEPSEEK_BASE_URL")]
    base_url: Option<String>,
    /// Max model requests per turn.
    #[arg(long, default_value_t = 50)]
    max_steps: u32,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Print an API bearer token to stdout (creates tokens on first use).
    Token {
        #[arg(long, value_enum, default_value = "admin")]
        scope: ScopeArg,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ScopeArg {
    Read,
    Run,
    Admin,
}

impl From<ScopeArg> for Scope {
    fn from(s: ScopeArg) -> Self {
        match s {
            ScopeArg::Read => Scope::Read,
            ScopeArg::Run => Scope::Run,
            ScopeArg::Admin => Scope::Admin,
        }
    }
}

/// Stand-in so the daemon (and `/health`) runs before a key is configured; turns fail with a
/// clear provider error instead.
#[derive(Debug)]
struct Unconfigured;

#[async_trait::async_trait]
impl Provider for Unconfigured {
    fn id(&self) -> &str {
        "unconfigured"
    }
    async fn models(&self) -> hk_llm::Result<Vec<ModelInfo>> {
        Ok(Vec::new())
    }
    async fn stream(&self, _req: ChatRequest, _cancel: CancellationToken) -> hk_llm::Result<EventStream> {
        Err(LlmError::Auth(format!("no DeepSeek API key: set {DEEPSEEK_ENV} or store secret `{DEEPSEEK_SECRET}`")))
    }
}

fn data_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("cannot locate home dir; pass --data-dir or set HOKKABAZ_HOME")?;
    Ok(PathBuf::from(home).join(".hokkabaz"))
}

/// OS keyring when it answers, else a 0600 file next to the DB.
fn secret_backend(data: &std::path::Path) -> Arc<dyn SecretStore> {
    let keyring = KeyringSecretStore::default();
    if keyring.get("hokkabazd-probe").is_ok() {
        Arc::new(keyring)
    } else {
        tracing::info!("OS keyring unavailable; using file secret store");
        Arc::new(FileSecretStore::new(data.join("secrets.json")))
    }
}

fn provider(secrets: &dyn SecretStore, base_url: Option<&str>) -> Arc<dyn Provider> {
    let key = std::env::var(DEEPSEEK_ENV)
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
        .or_else(|| secrets.get(DEEPSEEK_SECRET).ok().flatten().map(|s| s.expose().to_owned()));
    let Some(key) = key else {
        tracing::warn!("no DeepSeek API key configured; turns will fail until one is set");
        return Arc::new(Unconfigured);
    };
    match DeepSeek::new(key) {
        Ok(p) => Arc::new(match base_url {
            Some(u) => p.base_url(u),
            None => p,
        }),
        Err(e) => {
            tracing::error!(error = %e, "cannot build DeepSeek client");
            Arc::new(Unconfigured)
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let data = data_dir(cli.data_dir)?;
    std::fs::create_dir_all(&data).with_context(|| format!("create {}", data.display()))?;

    let store = Store::open(data.join("hk.db")).await.context("open store")?;
    let secrets = secret_backend(&data);
    let auth = Auth::load_or_create(&store, secrets.as_ref()).await.context("load api tokens")?;

    if let Some(Cmd::Token { scope }) = cli.cmd {
        let t = auth.token(scope.into()).context("token missing")?;
        // Explicit user request: the one place a token is printed.
        #[allow(clippy::print_stdout)]
        {
            println!("{}", t.expose());
        }
        return Ok(());
    }

    let runtime = LocalRuntime::new(LocalConfig {
        provider: provider(secrets.as_ref(), cli.base_url.as_deref()),
        sessions_dir: data.join("sessions"),
        spill_dir: data.join("spill"),
        default_model: cli.model,
        default_cwd: std::env::current_dir().ok(),
        max_steps: cli.max_steps,
    })
    .map_err(|e| anyhow::anyhow!("runtime: {e}"))?;

    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("bind {}", cli.listen))?;
    tracing::info!(addr = %listener.local_addr()?, data = %data.display(), "hokkabazd up; get a token with `hokkabazd token`");
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutting down");
    };
    hk_server::serve_listener(listener, AppState::new(Arc::new(runtime), auth), shutdown)
        .await
        .context("serve")
}
