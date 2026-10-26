//! Daemon assembly: open data dir (db, secrets, tokens), build the flow runtime and the
//! axum state.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use hk_llm::{CancellationToken, ChatRequest, EventStream, LlmError, ModelInfo, Provider};
use hk_server::{AgentRuntime, AppState, Auth, FlowApi, Pairing, Scope};
use hk_store::secrets::KeyringSecretStore;
use hk_store::{SecretStore, Store};

use crate::flow_runtime::{FlowConfig, FlowRuntime};
use crate::secrets::FileSecretStore;

/// Env var that pins the admin token (container secrets, provisioning). Also see
/// `--admin-token-file`.
pub const ADMIN_TOKEN_ENV: &str = "HOKKABAZ_ADMIN_TOKEN";

/// Secret name older daemons kept the DeepSeek key under; migrated to the provider key on start.
pub const LEGACY_DEEPSEEK_SECRET: &str = "deepseek";

/// Provider that has no models and fails every turn with a clear error: for tests and embedders
/// that never run a model.
#[derive(Debug)]
pub struct Unconfigured;

#[async_trait::async_trait]
impl Provider for Unconfigured {
    fn id(&self) -> &str {
        "unconfigured"
    }
    async fn models(&self) -> hk_llm::Result<Vec<ModelInfo>> {
        Ok(Vec::new())
    }
    async fn stream(
        &self,
        _req: ChatRequest,
        _cancel: CancellationToken,
    ) -> hk_llm::Result<EventStream> {
        Err(LlmError::Auth(
            "no model provider configured: add one under Models (or set e.g. ANTHROPIC_API_KEY)"
                .into(),
        ))
    }
}

pub struct Opened {
    pub store: Store,
    pub secrets: Arc<dyn SecretStore>,
    pub auth: Auth,
}

impl std::fmt::Debug for Opened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Opened")
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

/// OS keyring when it answers, else a 0600 file next to the DB.
pub fn secret_backend(data: &Path) -> Arc<dyn SecretStore> {
    let keyring = KeyringSecretStore::default();
    if keyring.get("hokkabazd-probe").is_ok() {
        Arc::new(keyring)
    } else {
        tracing::info!("OS keyring unavailable; using file secret store");
        Arc::new(FileSecretStore::new(data.join("secrets.json")))
    }
}

/// Open `<data>/hk.db` and load (or create) the API tokens. `admin_token` pins the admin
/// token to an operator-supplied value before the table loads.
pub async fn open(data: &Path, admin_token: Option<&str>) -> Result<Opened> {
    std::fs::create_dir_all(data).with_context(|| format!("create {}", data.display()))?;
    let store = Store::open(data.join("hk.db"))
        .await
        .context("open store")?;
    let secrets = secret_backend(data);
    open_with(store, secrets, admin_token).await
}

/// [`open`] over an existing store and secret backend (tests).
pub async fn open_with(
    store: Store,
    secrets: Arc<dyn SecretStore>,
    admin_token: Option<&str>,
) -> Result<Opened> {
    if let Some(t) = admin_token
        && Auth::bootstrap_tier_token(&store, secrets.as_ref(), Scope::Admin, t)
            .await
            .context("admin token")?
    {
        tracing::info!("admin token set from bootstrap value");
    }
    let auth = Auth::load_or_create(&store, secrets.as_ref())
        .await
        .context("load api tokens")?;
    Ok(Opened {
        store,
        secrets,
        auth,
    })
}

pub struct Assembled {
    pub state: AppState,
    pub flow: Arc<FlowRuntime>,
}

impl std::fmt::Debug for Assembled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Assembled")
            .field("flow", &self.flow)
            .finish_non_exhaustive()
    }
}

/// Wire the flow runtime and pairing into an [`AppState`], and activate stored workflows.
pub async fn assemble(
    o: Opened,
    runtime: Arc<dyn AgentRuntime>,
    mut flow_cfg: FlowConfig,
    public_url: &str,
) -> Result<Assembled> {
    // `hokkabaz.agent` runs full sessions on this instance's own agent runtime.
    if flow_cfg.agent_runner.is_none() {
        flow_cfg.agent_runner = Some(Arc::new(crate::agent_runner::RuntimeAgentRunner::new(
            runtime.clone(),
        )));
    }
    let base = FlowApi::new(o.store.clone(), o.secrets.clone()).with_jobs(runtime.jobs());
    let flow = FlowRuntime::start(base.clone(), flow_cfg).context("start flow runtime")?;
    let api = base
        .with_webhooks(flow.webhook_handler())
        .with_activator(flow.activator())
        .with_test_runner(flow.clone());
    let n = flow
        .activate_stored()
        .await
        .context("activate stored workflows")?;
    // Waits whose time passed while the daemon was down resume now, not at the first tick.
    let resumed = flow.resume_due(now_ms()).await;
    tracing::info!(active = n, resumed, "flow runtime up");
    // Agents can draft and test-run workflows on this instance.
    runtime.add_tools(hk_server::flow_tools(flow.clone(), o.store.clone()));
    #[cfg(feature = "plugins")]
    runtime.add_tools(flow.plugin_tools());
    let pairing = Pairing::new(&o.auth, public_url).context("pairing")?;
    let state = AppState::new(runtime, o.auth)
        .with_flow(api)
        .with_pairing(pairing);
    Ok(Assembled { state, flow })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
