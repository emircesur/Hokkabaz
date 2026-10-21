use crate::bus::Rejected;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("service already provided: {0}")]
    ServiceExists(String),
    #[error("service not provided: {0}")]
    ServiceMissing(String),
    #[error("interceptors not allowed for event domain `{0}`")]
    InterceptNotAllowed(String),
    #[error("plugin not loaded (unloaded while in use)")]
    PluginGone,
    #[error("plugin `{0}` failed to load: {1}")]
    PluginLoad(String, String),
    #[error(transparent)]
    Rejected(#[from] Rejected),
}
