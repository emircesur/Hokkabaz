//! Video Studio inside the in-process core: the shared [`StudioHost`] that the app's `api::studio`
//! functions, the agent's `studio_*` tools and the flow node all use.
//!
//! Studio runs on the machine that has ffmpeg (desktop, or a daemon). On Android the service
//! still starts, but ffmpeg calls fail with a clear error; a connected instance does the work
//! instead (`RemoteCore`).

use std::sync::{Arc, PoisonError, RwLock};

use hk_studio::{StudioError, StudioService};

use crate::error::{HkError, HkErrorKind};

pub use hk_studio::host::{AppBackends, SETTINGS_KEY, SharedSettings, StudioHost, StudioSettings};

static HOST: RwLock<Option<Arc<StudioHost>>> = RwLock::new(None);

/// Make this the process-wide Studio (called by `LocalCore::open`).
pub fn install(host: Arc<StudioHost>) {
    *HOST.write().unwrap_or_else(PoisonError::into_inner) = Some(host);
}

pub fn uninstall() {
    *HOST.write().unwrap_or_else(PoisonError::into_inner) = None;
}

pub fn host() -> Result<Arc<StudioHost>, HkError> {
    HOST.read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| {
            HkError::new(
                HkErrorKind::NotInitialized,
                "Studio needs the in-process core (core_init)",
            )
        })
}

pub fn service() -> Result<Arc<StudioService>, HkError> {
    Ok(host()?.svc.clone())
}

pub fn studio_err(e: StudioError) -> HkError {
    match e {
        StudioError::NotFound(m) => HkError::new(HkErrorKind::NotFound, m),
        StudioError::Invalid(m) => HkError::invalid(m),
        StudioError::Llm(m) | StudioError::Http(m) => HkError::new(HkErrorKind::Provider, m),
        other => HkError::internal(other.to_string()),
    }
}
