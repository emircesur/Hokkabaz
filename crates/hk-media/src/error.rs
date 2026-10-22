use std::path::PathBuf;

/// Errors from the ffmpeg/ffprobe wrapper.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// ffmpeg process wrapper is not available on this platform (Android).
    #[error("ffmpeg process wrapper unsupported on this platform")]
    Unsupported,
    #[error("binary not found: {}", .0.display())]
    BinaryNotFound(PathBuf),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{bin} exited with {code:?}: {stderr_tail}")]
    Failed {
        bin: String,
        code: Option<i32>,
        stderr_tail: String,
    },
    #[error("parse: {0}")]
    Parse(String),
    #[error("invalid spec: {0}")]
    InvalidSpec(String),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, MediaError>;
