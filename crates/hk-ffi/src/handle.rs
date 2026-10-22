//! [`CoreHandle`]: everything Dart can ask of a core, in DTO terms.
//!
//! [`crate::local::LocalCore`] runs the agent in-process. [`crate::remote::RemoteCore`] (REST +
//! WS to a `hokkabazd`) implements the same trait, so the `api::*` functions and the Dart side
//! never learn which one they talk to.

use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::dto::{
    CoreProfile, CreateSessionDto, ModelListDto, PreferencesDto, ProviderInputDto, SessionDetailDto,
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
    async fn set_api_key(&self, provider_id: String, key: Option<String>) -> Result<SettingsDto, HkError>;
    async fn set_preferences(&self, prefs: PreferencesDto) -> Result<SettingsDto, HkError>;

    async fn list_models(&self) -> Result<ModelListDto, HkError>;
    /// Re-fetch the models.dev catalog; failure leaves the cached one in place.
    async fn refresh_catalog(&self) -> Result<(), HkError>;

    async fn create_session(&self, req: CreateSessionDto) -> Result<SessionInfoDto, HkError>;
    async fn list_sessions(&self, limit: u32, offset: u32) -> Result<Vec<SessionInfoDto>, HkError>;
    async fn get_session(&self, session_id: &str) -> Result<SessionDetailDto, HkError>;
    async fn submit_turn(&self, session_id: &str, req: TurnRequestDto) -> Result<TurnAcceptedDto, HkError>;
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
    fn session_events(&self, session_id: &str) -> Result<BoxStream<'static, SessionEventDto>, HkError>;

    /// Stop background work. Idempotent.
    async fn shutdown(&self);
}

static CORE: RwLock<Option<Arc<dyn CoreHandle>>> = RwLock::new(None);
/// The in-process core, kept alive while a remote one is active so switching back is instant.
static LOCAL: RwLock<Option<Arc<dyn CoreHandle>>> = RwLock::new(None);

fn read(l: &RwLock<Option<Arc<dyn CoreHandle>>>) -> Option<Arc<dyn CoreHandle>> {
    l.read().unwrap_or_else(PoisonError::into_inner).clone()
}

fn write(l: &RwLock<Option<Arc<dyn CoreHandle>>>) -> std::sync::RwLockWriteGuard<'_, Option<Arc<dyn CoreHandle>>> {
    l.write().unwrap_or_else(PoisonError::into_inner)
}

/// The active core, or `NotInitialized`.
pub fn core() -> Result<Arc<dyn CoreHandle>, HkError> {
    read(&CORE).ok_or_else(|| HkError::new(HkErrorKind::NotInitialized, "core_init has not been called"))
}

/// The in-process core, or `NotInitialized`.
pub fn local() -> Result<Arc<dyn CoreHandle>, HkError> {
    read(&LOCAL).ok_or_else(|| HkError::new(HkErrorKind::NotInitialized, "core_init has not been called"))
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
