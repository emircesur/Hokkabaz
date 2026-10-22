//! [`CoreHandle`]: everything Dart can ask of a core, in DTO terms.
//!
//! [`crate::local::LocalCore`] runs the agent in-process. [`crate::remote::RemoteCore`] (REST +
//! WS to a `hokkabazd`) implements the same trait, so the `api::*` functions and the Dart side
//! never learn which one they talk to.

use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::dto::{
    CoreProfile, CreateSessionDto, FlowRunDetailDto, FlowRunEventDto, FlowRunModeDto,
    FlowRunSummaryDto, ModelListDto, PreferencesDto, ProviderInputDto, SessionDetailDto,
    SessionEventDto, SessionInfoDto, SettingsDto, TurnAcceptedDto, TurnRequestDto,
};
use crate::error::{HkError, HkErrorKind};

#[async_trait]
pub trait CoreHandle: Send + Sync + 'static {
    fn profile(&self) -> CoreProfile;

    async fn get_settings(&self) -> Result<SettingsDto, HkError>;
    /// Insert or update a provider; rebuilds the provider registry.
    async fn save_provider(&self, input: ProviderInputDto) -> Result<SettingsDto, HkError>;
    /// Custom providers only; built-ins are disabled via `save_provider`.
    async fn remove_provider(&self, id: String) -> Result<SettingsDto, HkError>;
    /// `None` / empty deletes the key. The key is never returned.
    async fn set_api_key(
        &self,
        provider_id: String,
        key: Option<String>,
    ) -> Result<SettingsDto, HkError>;
    async fn set_preferences(&self, prefs: PreferencesDto) -> Result<SettingsDto, HkError>;

    async fn list_models(&self) -> Result<ModelListDto, HkError>;
    /// Re-fetch the models.dev catalog; failure leaves the cached one in place.
    async fn refresh_catalog(&self) -> Result<(), HkError>;

    async fn create_session(&self, req: CreateSessionDto) -> Result<SessionInfoDto, HkError>;
    async fn list_sessions(&self, limit: u32, offset: u32) -> Result<Vec<SessionInfoDto>, HkError>;
    async fn get_session(&self, session_id: &str) -> Result<SessionDetailDto, HkError>;
    async fn submit_turn(
        &self,
        session_id: &str,
        req: TurnRequestDto,
    ) -> Result<TurnAcceptedDto, HkError>;
    async fn inject(&self, session_id: &str, text: String) -> Result<(), HkError>;
    async fn cancel(&self, session_id: &str) -> Result<(), HkError>;
    async fn respond_approval(
        &self,
        session_id: &str,
        approval_id: &str,
        approved: bool,
        reason: Option<String>,
    ) -> Result<(), HkError>;
    /// Live feed for one session; ends on `shutdown`.
    fn session_events(
        &self,
        session_id: &str,
    ) -> Result<BoxStream<'static, SessionEventDto>, HkError>;

    /// Start a test/manual run of a workflow (n8n JSON) and return its run id. The run continues in
    /// the background; follow it with `run_events`. Pin data in `pin_data_json` replaces the
    /// workflow's own `pinData` for this run. Bad input fails here, before an id exists.
    async fn run_workflow(
        &self,
        workflow_json: String,
        mode: FlowRunModeDto,
        trigger_node: Option<String>,
        input_items_json: Option<String>,
        pin_data_json: Option<String>,
    ) -> Result<String, HkError>;
    /// Replays then follows a run until its terminal event. Unknown id => `NotFound`.
    fn run_events(&self, run_id: &str) -> Result<BoxStream<'static, FlowRunEventDto>, HkError>;
    /// Newest first; `workflow_id` filters.
    async fn list_runs(
        &self,
        workflow_id: Option<String>,
        limit: u32,
    ) -> Result<Vec<FlowRunSummaryDto>, HkError>;
    async fn get_run(&self, run_id: &str) -> Result<FlowRunDetailDto, HkError>;
    /// Arm / disarm the workflow's triggers on the instance (uploads it first). Local core:
    /// `Unsupported`. Returns the resulting active flag.
    async fn set_workflow_active(
        &self,
        workflow_json: String,
        active: bool,
    ) -> Result<bool, HkError>;

    // ------------------------------------------------------------------ Studio

    /// Run a Studio method (see `hk_studio::host::StudioHost::call`). The local core runs it
    /// in-process; a remote core forwards it to the instance that has ffmpeg.
    async fn studio_rpc(
        &self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, HkError> {
        Err(HkError::new(
            HkErrorKind::Unsupported,
            "this core has no Video Studio",
        ))
    }

    /// Live change feed of one project (JSON `StudioEvent`s).
    async fn studio_events(&self, _id: &str) -> Result<BoxStream<'static, String>, HkError> {
        Err(HkError::new(
            HkErrorKind::Unsupported,
            "this core has no Video Studio",
        ))
    }

    /// Something a media player can open for a path found in a Studio result: the path itself on
    /// this device, a short-lived URL for a remote instance.
    async fn studio_media(&self, path: &str) -> Result<String, HkError> {
        Ok(path.to_owned())
    }

    /// Copy a local video to where the Studio runs; returns the path to import. Local: identity.
    /// `progress` gets `(sent, total)` bytes.
    async fn studio_upload(
        &self,
        local_path: &str,
        _progress: &(dyn Fn(u64, u64) + Send + Sync),
    ) -> Result<String, HkError> {
        Ok(local_path.to_owned())
    }

    /// Download a file produced by the Studio (render, export) to `dest` on this device.
    async fn studio_download(&self, _server_path: &str, _dest: &str) -> Result<(), HkError> {
        Err(HkError::new(
            HkErrorKind::Unsupported,
            "nothing to download from this core",
        ))
    }

    /// True when the Studio runs on another machine (paths in results are not local).
    fn studio_is_remote(&self) -> bool {
        false
    }

    /// Stop background work. Idempotent.
    async fn shutdown(&self);
}

static CORE: RwLock<Option<Arc<dyn CoreHandle>>> = RwLock::new(None);
/// The in-process core, kept alive while a remote one is active so switching back is instant.
static LOCAL: RwLock<Option<Arc<dyn CoreHandle>>> = RwLock::new(None);

fn read(l: &RwLock<Option<Arc<dyn CoreHandle>>>) -> Option<Arc<dyn CoreHandle>> {
    l.read().unwrap_or_else(PoisonError::into_inner).clone()
}

fn write(
    l: &RwLock<Option<Arc<dyn CoreHandle>>>,
) -> std::sync::RwLockWriteGuard<'_, Option<Arc<dyn CoreHandle>>> {
    l.write().unwrap_or_else(PoisonError::into_inner)
}

/// The active core, or `NotInitialized`.
pub fn core() -> Result<Arc<dyn CoreHandle>, HkError> {
    read(&CORE)
        .ok_or_else(|| HkError::new(HkErrorKind::NotInitialized, "core_init has not been called"))
}

/// The in-process core, or `NotInitialized`.
pub fn local() -> Result<Arc<dyn CoreHandle>, HkError> {
    read(&LOCAL)
        .ok_or_else(|| HkError::new(HkErrorKind::NotInitialized, "core_init has not been called"))
}

/// Install `core` as the in-process core and make it active. Returns the handles it displaced
/// (a hot restart re-inits without a shutdown); the caller shuts them down.
pub fn install_local(core: Arc<dyn CoreHandle>) -> Vec<Arc<dyn CoreHandle>> {
    let mut out: Vec<Arc<dyn CoreHandle>> = Vec::new();
    out.extend(write(&LOCAL).replace(core.clone()));
    out.extend(write(&CORE).replace(core));
    out
}

/// Make `core` the active one. Returns the previous active core when it is a remote one (the
/// caller shuts it down); the in-process core is never returned, it stays parked in `LOCAL`.
pub fn activate(core: Arc<dyn CoreHandle>) -> Option<Arc<dyn CoreHandle>> {
    let prev = write(&CORE).replace(core);
    let local = read(&LOCAL);
    prev.filter(|p| local.as_ref().is_none_or(|l| !Arc::ptr_eq(p, l)))
}

/// Active core is the in-process one.
pub fn local_active() -> bool {
    match (read(&CORE), read(&LOCAL)) {
        (Some(a), Some(l)) => Arc::ptr_eq(&a, &l),
        _ => false,
    }
}

/// Drop every core; the caller shuts the returned handles down.
pub fn take_all() -> Vec<Arc<dyn CoreHandle>> {
    let mut out: Vec<Arc<dyn CoreHandle>> = Vec::new();
    let active = write(&CORE).take();
    let local = write(&LOCAL).take();
    if let (Some(a), Some(l)) = (&active, &local) {
        if Arc::ptr_eq(a, l) {
            out.push(l.clone());
            return out;
        }
    }
    out.extend(active);
    out.extend(local);
    out
}
