/// Studio failure.
#[derive(Debug, thiserror::Error)]
pub enum StudioError {
    #[error("media: {0}")]
    Media(#[from] hk_media::MediaError),
    #[error("transcription: {0}")]
    Stt(#[from] hk_stt::SttError),
    #[error("llm: {0}")]
    Llm(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("http: {0}")]
    Http(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, StudioError>;
