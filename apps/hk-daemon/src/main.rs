//! `hokkabazd`: headless Hokkabaz instance. Serves the agent API (`/api/v1`), the n8n-shaped
//! REST surface and webhooks (`/webhook/*`), and runs active workflows (webhook + schedule
//! triggers, durable waits).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use hk_daemon::daemon::{ADMIN_TOKEN_ENV, LEGACY_DEEPSEEK_SECRET, assemble, open};
use hk_daemon::{FlowConfig, Opened};
use hk_providers::{HubOptions, ProviderHub, ProviderInput};
use hk_server::{Auth, LocalConfig, LocalRuntime, Pairing, Scope};
use hk_store::{SecretStore, Store};

#[derive(Debug, Parser)]
#[command(name = "hokkabazd", version, about = "Hokkabaz headless instance")]
struct Cli {
    /// Data directory (sessions, db, secrets). Default: ~/.hokkabaz.
    #[arg(long, global = true, env = "HOKKABAZ_HOME")]
    data_dir: Option<PathBuf>,
    /// Listen address. Loopback by default; bind wider only behind TLS termination.
    #[arg(
        long,
        global = true,
        env = "HOKKABAZ_LISTEN",
        default_value = "127.0.0.1:7878"
    )]
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
    /// `provider/model` for sessions created without one. Default: the one chosen in the app
    /// (instance settings), else `deepseek/deepseek-chat`.
    #[arg(long, env = "HOKKABAZ_MODEL")]
    model: Option<String>,
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
    /// Manage on-device models (`local/<id>` provider).
    #[cfg(feature = "local-llm")]
    Models {
        #[command(subcommand)]
        action: ModelsCmd,
    },
    /// Exit 0 when the instance at --listen answers `/api/v1/health` (container healthcheck).
    Healthcheck,
}

#[cfg(feature = "local-llm")]
#[derive(Debug, Subcommand)]
enum ModelsCmd {
    /// Catalog entries, installed state and whether they fit this machine.
    List,
    /// Download a catalog model (resumes a partial file).
    Pull { id: String },
    /// Register a GGUF file you already have.
    Add {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Delete an installed model.
    Remove { id: String },
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

/// Providers come from the instance settings (managed from the app) plus the environment:
/// `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`,
/// ... and `OLLAMA_HOST` enable their provider with no further setup.
async fn providers(
    store: &Store,
    secrets: &Arc<dyn SecretStore>,
    data: &std::path::Path,
    deepseek_base_url: Option<&str>,
) -> Result<Arc<ProviderHub>> {
    // Before multi-provider the DeepSeek key lived under the bare name `deepseek`.
    let key = hk_providers::settings::key_name("deepseek");
    if matches!(secrets.get(&key), Ok(None))
        && let Ok(Some(old)) = secrets.get(LEGACY_DEEPSEEK_SECRET)
    {
        store
            .put_secret(secrets.as_ref(), &key, old)
            .await
            .context("migrate DeepSeek key")?;
    }
    let hub = ProviderHub::open(
        store.clone(),
        HubOptions {
            secrets: secrets.clone(),
            extras: local_providers(data),
            catalog_path: data.join("models.json"),
            offline: None,
            env: true,
        },
    )
    .await
    .context("providers")?;
    if let Some(url) = deepseek_base_url {
        hub.save_provider(ProviderInput {
            id: "deepseek".into(),
            kind: "deepseek".into(),
            base_url: Some(url.to_owned()),
            enabled: true,
            models: Vec::new(),
        })
        .await
        .context("DeepSeek endpoint")?;
    }
    if hub.catalog_is_stale() {
        let hub = hub.clone();
        tokio::spawn(async move {
            if let Err(e) = hub.refresh_catalog().await {
                tracing::info!(error = %e, "model catalog refresh skipped");
            }
        });
    }
    Ok(hub)
}

#[cfg(not(feature = "local-llm"))]
fn local_providers(_data: &std::path::Path) -> Vec<Arc<dyn hk_llm::Provider>> {
    Vec::new()
}

/// On-device GGUF models served by this instance (`<data>/models`), provider id `local`.
#[cfg(feature = "local-llm")]
fn local_providers(data: &std::path::Path) -> Vec<Arc<dyn hk_llm::Provider>> {
    // GPU offload (builds with `local-llm-vulkan`/`-cuda`): HOKKABAZ_GPU=0 turns it off,
    // HOKKABAZ_GPU_DEVICE picks the card, HOKKABAZ_GPU_LAYERS caps offloaded layers.
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let mut g = hk_local::gpu::GpuSettings::default();
    if let Some(v) = env("HOKKABAZ_GPU") {
        g.enabled = !matches!(v.as_str(), "0" | "false" | "off");
    }
    if let Some(d) = env("HOKKABAZ_GPU_DEVICE").and_then(|v| v.parse().ok()) {
        g.device = d;
    }
    g.layers = env("HOKKABAZ_GPU_LAYERS").and_then(|v| v.parse().ok());
    hk_local::gpu::set(g);
    vec![Arc::new(hk_local::engine::LlamaProvider::new(
        hk_local::ModelStore::new(data.join("models")),
    ))]
}

fn admin_token(file: Option<&PathBuf>) -> Result<Option<String>> {
    if let Some(f) = file {
        let t = std::fs::read_to_string(f).with_context(|| format!("read {}", f.display()))?;
        return Ok(Some(t.trim().to_owned()));
    }
    Ok(std::env::var(ADMIN_TOKEN_ENV)
        .ok()
        .filter(|t| !t.trim().is_empty()))
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
    let addr = if listen.ip().is_unspecified() {
        SocketAddr::from(([127, 0, 0, 1], listen.port()))
    } else {
        listen
    };
    let probe = async {
        let mut s = tokio::net::TcpStream::connect(addr).await?;
        s.write_all(b"GET /api/v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await?;
        Ok::<_, std::io::Error>(buf)
    };
    let buf = tokio::time::timeout(Duration::from_secs(4), probe)
        .await
        .context("healthcheck timed out")??;
    if buf.starts_with(b"HTTP/1.1 200") {
        Ok(())
    } else {
        bail!(
            "unhealthy: {}",
            String::from_utf8_lossy(&buf[..buf.len().min(40)])
                .lines()
                .next()
                .unwrap_or("")
        )
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
    println!(
        "The pairing token is single-use and expires in {}s.",
        ttl.as_secs()
    );
    if public_url.contains("0.0.0.0") || public_url.contains("127.0.0.1") {
        println!("warning: {public_url} is not reachable from a phone; set --public-url.");
    }
    Ok(())
}

#[cfg(feature = "local-llm")]
#[allow(clippy::print_stdout)]
async fn models_cmd(data: &std::path::Path, action: ModelsCmd) -> Result<()> {
    use hk_local::{ModelStore, catalog};
    let store = ModelStore::new(data.join("models"));
    match action {
        ModelsCmd::List => {
            let dev = catalog::Device::probe();
            let have = store.installed();
            for e in catalog::builtin() {
                println!(
                    "{:<28} {:>5.1}B {:>6} MB  {:?}  {}",
                    e.id,
                    e.params_b,
                    e.size_bytes / 1_000_000,
                    catalog::fit(e.size_bytes, &dev),
                    if have.iter().any(|m| m.id == e.id) {
                        "installed"
                    } else {
                        ""
                    },
                );
            }
            for m in have.iter().filter(|m| m.custom) {
                println!("{:<28} custom  {}", m.id, m.path.display());
            }
        }
        ModelsCmd::Pull { id } => {
            let entry = catalog::builtin()
                .into_iter()
                .find(|e| e.id == id)
                .with_context(|| format!("no catalog model `{id}` (see `models list`)"))?;
            let done = store
                .download(
                    &entry,
                    |p| tracing::info!(done = p.done, total = p.total, "downloading"),
                    hk_llm::CancellationToken::new(),
                )
                .await
                .context("download")?;
            println!("installed: local/{} ({})", done.id, done.path.display());
        }
        ModelsCmd::Add { path, name } => {
            let m = store.add_custom(&path, name.as_deref()).context("add")?;
            println!("registered: local/{}", m.id);
        }
        ModelsCmd::Remove { id } => {
            store.remove(&id).context("remove")?;
            println!("removed: {id}");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut cli = Cli::parse();
    // PaaS hosts (Railway, Heroku-style) hand the port over as $PORT and expect all interfaces.
    if std::env::var_os("HOKKABAZ_LISTEN").is_none()
        && !std::env::args().any(|a| a == "--listen" || a.starts_with("--listen="))
        && let Some(port) = std::env::var("PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
    {
        cli.listen = SocketAddr::from(([0, 0, 0, 0], port));
    }
    if matches!(cli.cmd, Some(Cmd::Healthcheck)) {
        return healthcheck(cli.listen).await;
    }
    let data = data_dir(cli.data_dir)?;
    let public_url = cli
        .public_url
        .unwrap_or_else(|| format!("http://{}", cli.listen));
    #[cfg(feature = "local-llm")]
    if let Some(Cmd::Models { action }) = cli.cmd {
        return models_cmd(&data, action).await;
    }
    let opened = open(
        &data,
        admin_token(cli.admin_token_file.as_ref())?.as_deref(),
    )
    .await?;

    match cli.cmd {
        Some(Cmd::Token { scope }) => return print_token(&opened.auth, scope.into()),
        Some(Cmd::Pair { scope, ttl }) => {
            return print_pairing(
                &opened.auth,
                &public_url,
                scope.into(),
                Duration::from_secs(ttl),
            );
        }
        Some(Cmd::Healthcheck) => unreachable!("handled above"),
        #[cfg(feature = "local-llm")]
        Some(Cmd::Models { .. }) => unreachable!("handled above"),
        Some(Cmd::Serve) | None => {}
    }

    let Opened {
        store,
        secrets,
        auth,
    } = opened;
    let hub = providers(&store, &secrets, &data, cli.base_url.as_deref()).await?;
    let model_provider = hub.provider();
    let default_model = match cli.model.or(hub.default_model().await) {
        Some(m) if m.contains('/') => m,
        Some(m) => format!("deepseek/{m}"),
        None => "deepseek/deepseek-chat".to_owned(),
    };
    let runtime = LocalRuntime::new(LocalConfig {
        provider: model_provider.clone(),
        sessions_dir: data.join("sessions"),
        spill_dir: data.join("spill"),
        default_model,
        default_cwd: std::env::current_dir().ok(),
        max_steps: cli.max_steps,
    })
    .map_err(|e| anyhow::anyhow!("runtime: {e}"))?;

    let flow_cfg = FlowConfig {
        env_allowlist: cli.env_allow,
        webhook_base_url: Some(public_url.clone()),
        data_dir: Some(data.clone()),
        ..FlowConfig::default()
    };
    let pool = store.clone();
    #[cfg(feature = "studio")]
    let studio =
        hk_daemon::studio_api::build(&data, store.clone(), secrets.clone(), model_provider).await?;
    let runtime = Arc::new(runtime);
    let mut a = assemble(
        Opened {
            store,
            secrets,
            auth,
        },
        runtime.clone(),
        flow_cfg,
        &public_url,
    )
    .await?;
    #[cfg(feature = "studio")]
    {
        use hk_server::AgentRuntime as _;
        if let Some(reg) = runtime.jobs() {
            studio.svc.set_jobs(reg);
        }
        runtime.add_tools(hk_studio::studio_tools(studio.svc.clone()));
        a.state = a
            .state
            .with_studio(Arc::new(hk_daemon::studio_api::Api(studio)));
    }
    a.state = a.state.with_providers(hub);

    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("bind {}", cli.listen))?;
    tracing::info!(addr = %listener.local_addr()?, data = %data.display(), url = %public_url, "hokkabazd up; `hokkabazd token` prints an API token, `hokkabazd pair` a pairing QR");
    let shutdown = async {
        shutdown_signal().await;
        tracing::info!("shutting down");
    };
    let served = hk_server::serve_listener(listener, a.state, shutdown)
        .await
        .context("serve");
    // Stop schedulers first so nothing starts mid-close; then flush the WAL.
    a.flow.shutdown();
    pool.pool().close().await;
    served
}
