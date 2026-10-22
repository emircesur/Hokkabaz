//! Studio's headless pipeline, shared by the desktop FFI and remote daemon.
mod ai;
mod captions;
mod flows;
mod model;
mod playback;
mod service;
mod stock;
mod store;
mod tools;

pub use captions::{ass, srt, vtt};
pub use flows::register_studio_nodes;
pub use model::*;
pub use service::{Job, Studio};
pub use tools::studio_tools;

#[derive(Debug, thiserror::Error)]
pub enum StudioError {
    #[error("{0}")]
    Invalid(String),
    #[error("project changed while the operation was running; reload and retry")]
    Conflict,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Media(#[from] hk_media::MediaError),
    #[error(transparent)]
    Stt(#[from] hk_stt::SttError),
    #[error(transparent)]
    Llm(#[from] hk_llm::LlmError),
    #[error(transparent)]
    Secret(#[from] hk_store::SecretError),
    #[error(transparent)]
    Concat(#[from] hk_concat::ConcatError),
    #[error("network request failed: {0}")]
    Http(#[source] reqwest::Error),
}

impl From<reqwest::Error> for StudioError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error.without_url())
    }
}

pub type Result<T> = std::result::Result<T, StudioError>;

pub(crate) fn invalid(message: impl Into<String>) -> StudioError {
    StudioError::Invalid(message.into())
}
