//! `hokkabazd`: headless Hokkabaz instance. Serves the agent API (`/api/v1`), the n8n-shaped
//! REST surface and webhooks (`/webhook/*`), and runs active workflows (webhook + schedule
//! triggers, durable waits).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use hk_daemon::daemon::{ADMIN_TOKEN_ENV, DEEPSEEK_ENV, DEEPSEEK_SECRET, Unconfigured, assemble, open};
use hk_daemon::{FlowConfig, Opened};
use hk_llm::deepseek::DeepSeek;
use hk_llm::Provider;
use hk_server::{Auth, LocalConfig, LocalRuntime, Pairing, Scope};
use hk_store::SecretStore;

#[derive(Debug, Parser)]
#[command(name = "hokkabazd", version, about = "Hokkabaz headless instance")]
struct Cli {
    /// Data directory (sessions, db, secrets). Default: ~/.hokkabaz.
    #[arg(long, global = true, env = "HOKKABAZ_HOME")]
    data_dir: Option<PathBuf>,
    /// Listen address. Loopback by default; bind wider only behind TLS termination.
    #[arg(long, global = true, env = "HOKKABAZ_LISTEN", default_value = "127.0.0.1:7878")]
    listen: SocketAddr,
    /// Public base URL clients reach this instance at (webhook URLs, pairing QR).
    /// Default: http://<listen>.
    #[arg(long, global = true, env = "HOKKABAZ_PUBLIC_URL")]
    public_url: Option<String>,
    /// Read the admin API token from this file instead of generating one on first start.
    /// `HOKKABAZ_ADMIN_TOKEN` works too. Min 24 chars. Rotates the stored admin token.
    #[arg(long, global = true, env = "HOKKABAZ_ADMIN_TOKEN_FILE")]
    admin_token_file: Option<PathBuf>,
    /// Host env vars workflows may read via `$env` (comma separated). Default: none.
    #[arg(long, env = "HOKKABAZ_ENV_ALLOWLIST", value_delimiter = ',')]
    env_allow: Vec<String>,
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
    /// Run the instance (default).
    Serve,
    /// Print an API bearer token to stdout (creates tokens on first use).
    Token {
        #[arg(long, value_enum, default_value = "admin")]
        scope: ScopeArg,
    },
    /// Print a pairing QR (terminal) for the mobile app. The pairing token is single-use and
    /// short-lived; the app exchanges it for a `--scope` API token.
    Pair {
        #[arg(long, value_enum, default_value = "run")]
        scope: ScopeArg,
        /// Pairing token lifetime in seconds.
        #[arg(long, default_value_t = 300)]
        ttl: u64,
    },
    /// Exit 0 when the instance at --listen answers `/api/v1/health` (container healthcheck).
    Healthcheck,
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

fn data_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("cannot locate home dir; pass --data-dir or set HOKKABAZ_HOME")?;
    Ok(PathBuf::from(home).join(".hokkabaz"))
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

fn admin_token(file: Option<&PathBuf>) -> Result<Option<String>> {
    if let Some(f) = file {
        let t = std::fs::read_to_string(f).with_context(|| format!("read {}", f.display()))?;
        return Ok(Some(t.trim().to_owned()));
    }
    Ok(std::env::var(ADMIN_TOKEN_ENV).ok().filter(|t| !t.trim().is_empty()))
}

/// SIGINT or SIGTERM (docker stop, systemd).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// Plain-TCP GET so the image needs no curl.
async fn healthcheck(listen: SocketAddr) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = if listen.ip().is_unspecified() { SocketAddr::from(([127, 0, 0, 1], listen.port())) } else { listen };
    let probe = async {
        let mut s = tokio::net::TcpStream::connect(addr).await?;
        s.write_all(b"GET /api/v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await?;
        Ok::<_, std::io::Error>(buf)
    };
    let buf = tokio::time::timeout(Duration::from_secs(4), probe).await.context("healthcheck timed out")??;
    if buf.starts_with(b"HTTP/1.1 200") {
        Ok(())
    } else {
        bail!("unhealthy: {}", String::from_utf8_lossy(&buf[..buf.len().min(40)]).lines().next().unwrap_or(""))
    }
}

#[allow(clippy::print_stdout)]
fn print_token(auth: &Auth, scope: Scope) -> Result<()> {
    let t = auth.token(scope).context("token missing")?;
    // Explicit user request: the one place an API token is printed.
    println!("{}", t.expose());
    Ok(())
}

#[allow(clippy::print_stdout)]
fn print_pairing(auth: &Auth, public_url: &str, scope: Scope, ttl: Duration) -> Result<()> {
    let m = Pairing::new(auth, public_url)?.with_ttl(ttl).mint(scope)?;
    println!("{}", m.qr_terminal()?);
    println!("Scan with the Hokkabaz app (scope: {}).", scope.as_str());
    println!("url:   {}", m.url);
    println!("token: {}", m.token);
    println!("The pairing token is single-use and expires in {}s.", ttl.as_secs());
    if public_url.contains("0.0.0.0") || public_url.contains("127.0.0.1") {
        println!("warning: {public_url} is not reachable from a phone; set --public-url.");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    if matches!(cli.cmd, Some(Cmd::Healthcheck)) {
        return healthcheck(cli.listen).await;
    }
    let data = data_dir(cli.data_dir)?;
    let public_url = cli.public_url.unwrap_or_else(|| format!("http://{}", cli.listen));
    let opened = open(&data, admin_token(cli.admin_token_file.as_ref())?.as_deref()).await?;

    match cli.cmd {
        Some(Cmd::Token { scope }) => return print_token(&opened.auth, scope.into()),
        Some(Cmd::Pair { scope, ttl }) => {
            return print_pairing(&opened.auth, &public_url, scope.into(), Duration::from_secs(ttl));
        }
        Some(Cmd::Healthcheck) => unreachable!("handled above"),
        Some(Cmd::Serve) | None => {}
    }

    let Opened { store, secrets, auth } = opened;
    let studio_provider = provider(secrets.as_ref(), cli.base_url.as_deref());
    let runtime = LocalRuntime::new(LocalConfig {
        provider: studio_provider.clone(),
        sessions_dir: data.join("sessions"),
        spill_dir: data.join("spill"),
        default_model: cli.model,
        default_cwd: std::env::current_dir().ok(),
        max_steps: cli.max_steps,
    })
    .map_err(|e| anyhow::anyhow!("runtime: {e}"))?;

    let studio = hk_studio::Studio::new(&data, studio_provider, secrets.clone())?;
    runtime.attach_studio(studio.clone());

    let flow_cfg = FlowConfig {
        env_allowlist: cli.env_allow,
        webhook_base_url: Some(public_url.clone()),
        ..FlowConfig::default()
    };
    let pool = store.clone();
    let a = assemble(Opened { store, secrets, auth }, Arc::new(runtime), flow_cfg, &public_url).await?;

    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("bind {}", cli.listen))?;
    tracing::info!(addr = %listener.local_addr()?, data = %data.display(), url = %public_url, "hokkabazd up; `hokkabazd token` prints an API token, `hokkabazd pair` a pairing QR");
    let shutdown = async {
        shutdown_signal().await;
        tracing::info!("shutting down");
    };
    let served = hk_server::serve_listener(listener, a.state, shutdown).await.context("serve");
    studio.shutdown();
    // Stop schedulers first so nothing starts mid-close; then flush the WAL.
    a.flow.shutdown();
    pool.pool().close().await;
    served
}
