use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("write denied outside sandbox roots: {}", .0.display())]
    WriteDenied(PathBuf),
    #[error("path must be absolute: {}", .0.display())]
    NotAbsolute(PathBuf),
    #[error("command not in allowlist: {0}")]
    CommandNotAllowed(String),
    #[error("empty argv")]
    EmptyArgv,
    #[error("sandbox backend unavailable: {0}")]
    Unavailable(String),
    #[error("sandbox setup failed: {0}")]
    Setup(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("policy encoding: {0}")]
    Encoding(#[from] serde_json::Error),
}
