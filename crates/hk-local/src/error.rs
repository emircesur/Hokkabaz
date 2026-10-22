#[derive(Debug, thiserror::Error)]
pub enum LocalError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("download: {0}")]
    Download(String),
    #[error("checksum mismatch for {0}: the file was removed, try again")]
    Checksum(String),
    #[error("unknown model `{0}`")]
    UnknownModel(String),
    #[error("{0}")]
    Invalid(String),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, LocalError>;
