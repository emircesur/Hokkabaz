//! [`LocalCore`]: the in-process [`CoreHandle`]. Wires settings + secrets + provider registry
//! into one `hk_server::LocalRuntime` (the agent wiring shared with `hokkabazd`).
//!
//! Layout under the data dir (same as `hk`): `sessions/`, `spill/`, `hk.db`, `models.json`
//! (catalog cache), `secrets.json` (only when the OS keyring is unusable), `workspace/`
//! (default tool root).
//!
//! Provider changes rebuild the registry and swap the router behind a [`Reloadable`], so live
//! agents pick the new providers on their next request without being restarted.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use hk_llm::catalog::{Catalog, CatalogStore};
use hk_llm::registry::{KeyRef, ProviderRegistry, build_router};
use hk_llm::{CancellationToken, ChatRequest, EventStream, ModelInfo, Provider};
use hk_server::{
    AgentRuntime, ApprovalDecision, CreateSession, LocalConfig, LocalRuntime, SessionInfo,
    TurnMode, TurnRequest,
};
use hk_session::EventKind;
use hk_store::{Secret, SecretStore, Store};
use tokio::sync::{Mutex, broadcast};

use crate::dto::{
    CoreProfile, CreateSessionDto, FlowRunDetailDto, FlowRunEventDto, FlowRunModeDto,
    FlowRunSummaryDto, MessageDto, ModeDto, ModelListDto, PendingApprovalDto, PreferencesDto,
    ProviderDto, ProviderErrorDto, ProviderInputDto, RoleDto, SessionDetailDto, SessionEventDto,
    SessionEventKind, SessionInfoDto, SettingsDto, ToolCallDto, TurnAcceptedDto, TurnRequestDto,
};
use crate::error::{HkError, HkErrorKind};
use crate::flow_runs::LocalFlows;
use crate::handle::CoreHandle;
use crate::settings::{self, Stored};
use crate::{events, secrets};

/// Overridable pieces; `Default` is what `core_init` uses.
pub struct CoreOptions {
    /// Secret backend; default picks keyring/file by profile.
    pub secrets: Option<Arc<dyn SecretStore>>,
    /// Prebuilt providers always present in the registry (tests, plugins).
    pub extra_providers: Vec<Arc<dyn Provider>>,
    /// Fetch the models.dev catalog in the background when the cache is stale.
    pub refresh_catalog: bool,
}

impl Default for CoreOptions {
    fn default() -> Self {
        Self {
            secrets: None,
            extra_providers: Vec::new(),
            refresh_catalog: true,
        }
    }
}

impl std::fmt::Debug for CoreOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreOptions")
            .field("refresh_catalog", &self.refresh_catalog)
            .finish_non_exhaustive()
    }
}

/// Provider whose target can be swapped while agents hold it.
struct Reloadable(RwLock<Arc<dyn Provider>>);

impl Reloadable {
    fn current(&self) -> Arc<dyn Provider> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn swap(&self, p: Arc<dyn Provider>) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = p;
    }
}

#[async_trait]
impl Provider for Reloadable {
    fn id(&self) -> &str {
        "hk-core"
    }

    async fn models(&self) -> hk_llm::Result<Vec<ModelInfo>> {
        self.current().models().await
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> hk_llm::Result<EventStream> {
        self.current().stream(req, cancel).await
    }
}

/// Result of the last registry build.
struct Built {
    registry: ProviderRegistry,
    /// Provider id -> why it is unusable.
    errors: HashMap<String, String>,
}

struct Inner {
    profile: CoreProfile,
    data_dir: PathBuf,
    store: Store,
    secrets: Arc<dyn SecretStore>,
    /// Serialises settings read-modify-write.
    settings: Mutex<Stored>,
    built: RwLock<Arc<Built>>,
    catalog: RwLock<Option<Arc<Catalog>>>,
    catalog_store: CatalogStore,
    reload: Arc<Reloadable>,
    runtime: LocalRuntime,
    flows: LocalFlows,
    extras: Vec<Arc<dyn Provider>>,
    shutdown: CancellationToken,
}

/// In-process core. Cheap to clone-share via `Arc`.
pub struct LocalCore {
    i: Arc<Inner>,
}

impl std::fmt::Debug for LocalCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalCore")
            .field("data_dir", &self.i.data_dir)
            .finish_non_exhaustive()
    }
}

impl LocalCore {
    /// Open (creating) the data dir, load settings, build providers, start the runtime.
    pub async fn open(
        data_dir: PathBuf,
        profile: CoreProfile,
        opts: CoreOptions,
    ) -> Result<Arc<Self>, HkError> {
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| HkError::invalid(format!("data_dir {}: {e}", data_dir.display())))?;
        let store = Store::open(data_dir.join("hk.db")).await?;
        let secrets = opts
            .secrets
            .unwrap_or_else(|| secrets::select(profile, &data_dir));
        let stored = Stored::load(store.get_setting(settings::SETTINGS_KEY).await?.as_deref())?;

        let workspace = effective_workspace(&stored, &data_dir);
        std::fs::create_dir_all(&workspace)
            .map_err(|e| HkError::internal(format!("workspace: {e}")))?;

        let catalog_store = CatalogStore::new(data_dir.join("models.json"));
        let catalog = catalog_store.load_cached().map(Arc::new);
        let stale = !catalog_store.is_fresh();

        let reload = Arc::new(Reloadable(RwLock::new(Arc::new(hk_llm::Router::new()))));
        let runtime = LocalRuntime::new(LocalConfig {
            provider: reload.clone(),
            sessions_dir: data_dir.join("sessions"),
            spill_dir: data_dir.join("spill"),
            // Sessions always get an explicit model from `create_session`; this is only a floor.
            default_model: "deepseek/deepseek-chat".to_owned(),
            default_cwd: Some(workspace),
            max_steps: 50,
        })?;

        let creds = Arc::new(crate::credentials::Credentials {
            store: store.clone(),
            secrets: secrets.clone(),
        });
        let flows = LocalFlows::new(&data_dir, creds.clone())?;

        // On-device models: the `local` provider is always registered (it lists only what is
        // downloaded); the offline fallback chain is added in `rebuild`.
        let local_ctx = crate::local_models::install(&data_dir, store.clone()).await?;
        let mut extras = opts.extra_providers;
        if let Some(p) = crate::local_models::provider(&local_ctx) {
            extras.push(p);
        }

        // Video Studio: shared service + the agent's studio_* tools.
        let studio_settings: crate::studio::SharedSettings =
            Arc::new(RwLock::new(crate::studio::StudioSettings::load(
                store
                    .get_setting(crate::studio::SETTINGS_KEY)
                    .await?
                    .as_deref(),
            )));
        let studio_svc = Arc::new(hk_studio::StudioService::new(
            data_dir.join("studio"),
            hk_media::Tools::from_env(),
            Arc::new(crate::studio::AppBackends {
                provider: reload.clone(),
                secrets: secrets.clone(),
                settings: studio_settings.clone(),
                data_dir: data_dir.clone(),
            }),
        ));
        runtime.add_tools(hk_studio::studio_tools(studio_svc.clone()));
        crate::studio::install(Arc::new(crate::studio::StudioHost {
            svc: studio_svc,
            settings: studio_settings,
            store: store.clone(),
            secrets: secrets.clone(),
        }));
        crate::n8n_sync::install(creds.clone()).await?;
        crate::credentials::install(creds);

        // Plugins + MCP servers: their tools join the agent's tool set, plugin nodes the registry.
        let tool_rt = runtime.clone();
        let plugin_mgr = crate::plugins::Manager::new(
            &data_dir,
            secrets.clone(),
            Arc::new(move |t| tool_rt.add_tools(t)),
        )?;
        plugin_mgr.load_all();
        crate::plugins::install(plugin_mgr.clone());
        tokio::spawn(async move { plugin_mgr.mcp_connect_all().await });

        let core = Arc::new(Self {
            i: Arc::new(Inner {
                profile,
                data_dir,
                store,
                secrets,
                settings: Mutex::new(stored.clone()),
                built: RwLock::new(Arc::new(Built {
                    registry: ProviderRegistry::new(),
                    errors: HashMap::new(),
                })),
                catalog: RwLock::new(catalog),
                catalog_store,
                reload,
                runtime,
                flows,
                extras,
                shutdown: CancellationToken::new(),
            }),
        });
        core.i.rebuild(&stored)?;
        {
            let i = core.i.clone();
            local_ctx.set_rebuild(Arc::new(move || {
                let i = i.clone();
                Box::pin(async move {
                    let s = i.settings.lock().await.clone();
                    if let Err(e) = i.rebuild(&s) {
                        tracing::warn!(error = %e, "provider rebuild failed");
                    }
                })
            }));
        }

        if opts.refresh_catalog && stale {
            let i = core.i.clone();
            tokio::spawn(async move {
                tokio::select! {
                    () = i.shutdown.cancelled() => {}
                    r = i.refresh_catalog() => {
                        if let Err(e) = r {
                            // Offline is normal; the cached/empty catalog keeps working.
                            tracing::info!(error = %e, "catalog refresh skipped");
                        }
                    }
                }
            });
        }
        Ok(core)
    }
}

fn effective_workspace(s: &Stored, data_dir: &std::path::Path) -> PathBuf {
    s.workspace_root
        .as_deref()
        .map_or_else(|| data_dir.join("workspace"), PathBuf::from)
}

impl Inner {
    fn has_key(&self, provider_id: &str) -> bool {
        matches!(
            self.secrets.get(&settings::key_name(provider_id)),
            Ok(Some(_))
        )
    }

    /// Rebuild providers from `stored` (lenient: one bad provider does not hide the rest).
    fn rebuild(&self, stored: &Stored) -> Result<(), HkError> {
        let configs = settings::to_configs(stored, |id| self.has_key(id));
        let secrets = self.secrets.clone();
        let keys = move |r: &KeyRef| {
            secrets
                .get(&r.0)
                .ok()
                .flatten()
                .map(|s| s.expose().to_owned())
        };
        let (mut registry, errs) = ProviderRegistry::build_lenient(&configs, &keys);
        for p in &self.extras {
            registry = registry.with_provider(p.clone());
        }
        let errors = errs
            .iter()
            .map(|e| {
                (
                    settings::registry_error_provider(e).to_owned(),
                    e.to_string(),
                )
            })
            .collect::<HashMap<_, _>>();
        // Offline fallback: the default model's chain ends in the chosen on-device model.
        let mut routes = Vec::new();
        if let (Some(local), Some(def)) = (
            crate::local_models::fallback(),
            stored.default_model.clone(),
        ) && !def.starts_with("local/")
            && registry
                .providers()
                .any(|p| p.id() == hk_local::PROVIDER_ID)
        {
            routes.push(hk_llm::RouteConfig {
                key: Some(def.clone()),
                refs: vec![def, format!("{}/{local}", hk_local::PROVIDER_ID)],
            });
        }
        let mut router = build_router(&registry, routes)
            .map_err(|e| HkError::new(HkErrorKind::Provider, e.to_string()))?;
        if let Some(c) = self
            .catalog
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            router = router.catalog(c);
        }
        self.reload.swap(Arc::new(router));
        *self.built.write().unwrap_or_else(PoisonError::into_inner) =
            Arc::new(Built { registry, errors });
        Ok(())
    }

    async fn refresh_catalog(&self) -> Result<(), HkError> {
        let client = hk_llm::openai::default_client()
            .map_err(|e| HkError::new(HkErrorKind::Provider, e.to_string()))?;
        let cat = self
            .catalog_store
            .refresh(&client)
            .await
            .map_err(|e| HkError::new(HkErrorKind::Provider, format!("catalog: {e}")))?;
        *self.catalog.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(cat));
        let stored = self.settings.lock().await.clone();
        self.rebuild(&stored)
    }

    fn settings_dto(&self, s: &Stored) -> SettingsDto {
        let built = self
            .built
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        SettingsDto {
            providers: s
                .providers
                .iter()
                .map(|p| ProviderDto {
                    id: p.id.clone(),
                    kind: settings::kind_dto(&p.kind),
                    base_url: p.base_url.clone(),
                    enabled: p.enabled,
                    has_key: self.has_key(&p.id),
                    builtin: settings::is_builtin(&p.id),
                    models: p.models.clone(),
                    error: p
                        .enabled
                        .then(|| built.errors.get(&p.id).cloned())
                        .flatten(),
                })
                .collect(),
            default_model: s.default_model.clone(),
            mode: s.mode(),
            workspace_root: effective_workspace(s, &self.data_dir).display().to_string(),
            profile: self.profile,
            data_dir: self.data_dir.display().to_string(),
        }
    }

    /// Persist, rebuild providers, return the new view.
    async fn commit(&self, s: &Stored) -> Result<SettingsDto, HkError> {
        self.store
            .set_setting(settings::SETTINGS_KEY, &serde_json::to_string(s)?)
            .await?;
        self.rebuild(s)?;
        Ok(self.settings_dto(s))
    }
}

pub(crate) fn session_dto(s: SessionInfo) -> SessionInfoDto {
    SessionInfoDto {
        id: s.id,
        title: s.title,
        model: s.model,
        cwd: s.cwd,
        created_ms: s.created_ms,
        updated_ms: s.updated_ms,
        event_count: events::to_i64(s.event_count),
        turn_count: events::to_i64(s.turn_count),
        running: s.running,
    }
}

pub(crate) fn message_dto(m: hk_llm::Message) -> MessageDto {
    MessageDto {
        role: match m.role {
            hk_llm::Role::System => RoleDto::System,
            hk_llm::Role::User => RoleDto::User,
            hk_llm::Role::Assistant => RoleDto::Assistant,
            hk_llm::Role::Tool => RoleDto::Tool,
        },
        content: m.content,
        reasoning: m.reasoning,
        tool_calls: m
            .tool_calls
            .into_iter()
            .map(|c| ToolCallDto {
                call_id: c.id,
                name: c.name,
                arguments_json: c.arguments.to_string(),
            })
            .collect(),
        tool_call_id: m.tool_call_id,
    }
}

/// Parked approvals of a session, with the request details from the log.
pub(crate) fn pending_approvals(
    events: &[hk_session::SessionEvent],
    pending: &[String],
) -> Vec<PendingApprovalDto> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::ApprovalRequest {
                approval_id,
                call_id,
                tool,
                detail,
            } if pending.contains(approval_id) => Some(PendingApprovalDto {
                approval_id: approval_id.clone(),
                call_id: call_id.clone(),
                tool: tool.clone(),
                detail: detail.clone(),
            }),
            _ => None,
        })
        .collect()
}

#[async_trait]
impl CoreHandle for LocalCore {
    fn profile(&self) -> CoreProfile {
        self.i.profile
    }

    async fn get_settings(&self) -> Result<SettingsDto, HkError> {
        let s = self.i.settings.lock().await;
        Ok(self.i.settings_dto(&s))
    }

    async fn save_provider(&self, input: ProviderInputDto) -> Result<SettingsDto, HkError> {
        let mut s = self.i.settings.lock().await;
        let mut next = s.clone();
        next.upsert(input)?;
        let dto = self.i.commit(&next).await?;
        *s = next;
        Ok(dto)
    }

    async fn remove_provider(&self, id: String) -> Result<SettingsDto, HkError> {
        let mut s = self.i.settings.lock().await;
        let mut next = s.clone();
        next.remove(&id)?;
        // Drop the orphaned key too; a failure here must not block removal.
        if let Err(e) = self
            .i
            .store
            .delete_secret(&*self.i.secrets, &settings::key_name(&id))
            .await
        {
            tracing::warn!(provider = %id, error = %e, "could not delete provider key");
        }
        let dto = self.i.commit(&next).await?;
        *s = next;
        Ok(dto)
    }

    async fn set_api_key(
        &self,
        provider_id: String,
        key: Option<String>,
    ) -> Result<SettingsDto, HkError> {
        let s = self.i.settings.lock().await;
        if s.find(&provider_id).is_none() {
            return Err(HkError::new(
                HkErrorKind::NotFound,
                format!("no provider `{provider_id}`"),
            ));
        }
        let name = settings::key_name(&provider_id);
        match key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
            Some(k) => {
                self.i
                    .store
                    .put_secret(&*self.i.secrets, &name, Secret::new(k))
                    .await?
            }
            None => self.i.store.delete_secret(&*self.i.secrets, &name).await?,
        }
        self.i.rebuild(&s)?;
        Ok(self.i.settings_dto(&s))
    }

    async fn set_preferences(&self, prefs: PreferencesDto) -> Result<SettingsDto, HkError> {
        let mut s = self.i.settings.lock().await;
        let mut next = s.clone();
        if let Some(m) = &prefs.default_model {
            settings::validate_model_ref(m)?;
        }
        next.default_model = prefs.default_model;
        next.set_mode(prefs.mode);
        next.workspace_root = match prefs.workspace_root {
            Some(p) => Some(settings::prepare_workspace(&p)?.display().to_string()),
            None => None,
        };
        let dto = self.i.commit(&next).await?;
        *s = next;
        Ok(dto)
    }

    async fn list_models(&self) -> Result<ModelListDto, HkError> {
        let built = self
            .i
            .built
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let catalog = self
            .i
            .catalog
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let list = built.registry.list_models(catalog.as_deref()).await;
        let mut errors: Vec<ProviderErrorDto> = built
            .errors
            .iter()
            .map(|(id, m)| ProviderErrorDto {
                provider_id: id.clone(),
                message: m.clone(),
            })
            .collect();
        errors.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));
        errors.extend(list.errors.into_iter().map(|(id, e)| ProviderErrorDto {
            provider_id: id,
            message: e.to_string(),
        }));
        Ok(ModelListDto {
            models: list.models.iter().map(settings::model_dto).collect(),
            errors,
        })
    }

    async fn refresh_catalog(&self) -> Result<(), HkError> {
        self.i.refresh_catalog().await
    }

    async fn create_session(&self, req: CreateSessionDto) -> Result<SessionInfoDto, HkError> {
        let (model, cwd) = {
            let s = self.i.settings.lock().await;
            let model = req
                .model
                .or_else(|| s.default_model.clone())
                .ok_or_else(|| HkError::invalid("no model: pass one or set a default model"))?;
            settings::validate_model_ref(&model)?;
            let cwd = match req.workspace_root {
                Some(p) => settings::prepare_workspace(&p)?,
                None => effective_workspace(&s, &self.i.data_dir),
            };
            (model, cwd)
        };
        let info = self
            .i
            .runtime
            .create_session(CreateSession {
                cwd: Some(cwd.display().to_string()),
                model: Some(model),
                title: req.title,
            })
            .await?;
        Ok(session_dto(info))
    }

    async fn list_sessions(&self, limit: u32, offset: u32) -> Result<Vec<SessionInfoDto>, HkError> {
        Ok(self
            .i
            .runtime
            .list_sessions(limit, offset)
            .await?
            .into_iter()
            .map(session_dto)
            .collect())
    }

    async fn get_session(&self, session_id: &str) -> Result<SessionDetailDto, HkError> {
        let d = self.i.runtime.session(session_id, None).await?;
        let pending_approvals = pending_approvals(&d.events, &d.pending_approvals);
        Ok(SessionDetailDto {
            session: session_dto(d.session),
            messages: d.messages.into_iter().map(message_dto).collect(),
            pending_approvals,
        })
    }

    async fn submit_turn(
        &self,
        session_id: &str,
        req: TurnRequestDto,
    ) -> Result<TurnAcceptedDto, HkError> {
        if req.prompt.trim().is_empty() {
            return Err(HkError::invalid("prompt must not be empty"));
        }
        if let Some(m) = &req.model {
            settings::validate_model_ref(m)?;
        }
        let mode = match req.mode {
            Some(m) => m,
            None => self.i.settings.lock().await.mode(),
        };
        let accepted = self
            .i
            .runtime
            .submit(
                session_id,
                TurnRequest {
                    prompt: req.prompt,
                    mode: Some(match mode {
                        ModeDto::Build => TurnMode::Build,
                        ModeDto::Plan => TurnMode::Plan,
                    }),
                    model: req.model,
                },
            )
            .await?;
        Ok(TurnAcceptedDto {
            session_id: accepted.session_id,
            queued: accepted.queued,
        })
    }

    async fn inject(&self, session_id: &str, text: String) -> Result<(), HkError> {
        Ok(self.i.runtime.inject(session_id, text).await?)
    }

    async fn cancel(&self, session_id: &str) -> Result<(), HkError> {
        Ok(self.i.runtime.cancel(session_id).await?)
    }

    async fn respond_approval(
        &self,
        session_id: &str,
        approval_id: &str,
        approved: bool,
        reason: Option<String>,
    ) -> Result<(), HkError> {
        let d = ApprovalDecision {
            approved,
            reason,
            session_id: Some(session_id.to_owned()),
        };
        Ok(self.i.runtime.respond_approval(approval_id, d).await?)
    }

    fn session_events(
        &self,
        session_id: &str,
    ) -> Result<BoxStream<'static, SessionEventDto>, HkError> {
        // Subscribe now (not on first poll) so nothing between this call and the first `next()` is lost.
        let st = Feed {
            rx: self.i.runtime.subscribe(),
            session_id: session_id.to_owned(),
            queue: VecDeque::new(),
            shutdown: self.i.shutdown.clone(),
        };
        Ok(stream::unfold(st, Feed::next).boxed())
    }

    async fn run_workflow(
        &self,
        workflow_json: String,
        mode: FlowRunModeDto,
        trigger_node: Option<String>,
        input_items_json: Option<String>,
        pin_data_json: Option<String>,
    ) -> Result<String, HkError> {
        self.i.flows.start(
            &workflow_json,
            mode,
            trigger_node.as_deref(),
            input_items_json.as_deref(),
            pin_data_json.as_deref(),
        )
    }

    fn run_events(&self, run_id: &str) -> Result<BoxStream<'static, FlowRunEventDto>, HkError> {
        self.i.flows.events(run_id)
    }

    async fn list_runs(
        &self,
        workflow_id: Option<String>,
        limit: u32,
    ) -> Result<Vec<FlowRunSummaryDto>, HkError> {
        self.i.flows.list(workflow_id, limit).await
    }

    async fn get_run(&self, run_id: &str) -> Result<FlowRunDetailDto, HkError> {
        self.i.flows.get(run_id).await
    }

    async fn set_workflow_active(
        &self,
        _workflow_json: String,
        _active: bool,
    ) -> Result<bool, HkError> {
        Err(HkError::new(
            HkErrorKind::Unsupported,
            "Activating a workflow needs a connected instance; this app only runs it on demand",
        ))
    }

    async fn studio_rpc(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, HkError> {
        crate::studio::host()?
            .call(method, params)
            .await
            .map_err(crate::studio::studio_err)
    }

    async fn studio_events(
        &self,
        id: &str,
    ) -> Result<futures::stream::BoxStream<'static, String>, HkError> {
        crate::studio::host()?
            .events(id)
            .await
            .map_err(crate::studio::studio_err)
    }

    async fn shutdown(&self) {
        self.i.shutdown.cancel();
        crate::studio::uninstall();
        crate::local_models::uninstall();
        crate::plugins::uninstall();
        crate::credentials::uninstall();
        crate::n8n_sync::uninstall();
        // Stop running turns so nothing keeps calling providers after the app tore the core down.
        if let Ok(rows) = self.i.runtime.list_sessions(u32::MAX, 0).await {
            for s in rows.into_iter().filter(|s| s.running) {
                let _ = self.i.runtime.cancel(&s.id).await;
            }
        }
    }
}

struct Feed {
    rx: broadcast::Receiver<hk_server::ServerEvent>,
    session_id: String,
    queue: VecDeque<SessionEventDto>,
    shutdown: CancellationToken,
}

impl Feed {
    async fn next(mut self) -> Option<(SessionEventDto, Self)> {
        loop {
            if let Some(e) = self.queue.pop_front() {
                return Some((e, self));
            }
            let ev = tokio::select! {
                () = self.shutdown.cancelled() => return None,
                r = self.rx.recv() => match r {
                    Ok(ev) => ev,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        self.queue.push_back(SessionEventDto {
                            text: Some(format!("fell behind, {n} events dropped; reload the session")),
                            ..SessionEventDto::new(SessionEventKind::Error)
                        });
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
            };
            if ev.session_id == self.session_id {
                self.queue.extend(events::map(ev));
            }
        }
    }
}
