//! Error type for the session crate.

use std::path::PathBuf;

/// Failures from log IO, the index, or session operations.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Filesystem failure.
    #[error("io error at {path}: {source}")]
    Io {
        /// File or directory involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// Log line is not a valid event, or the log is internally inconsistent.
    #[error("corrupt log {path} line {line}: {msg}")]
    Corrupt {
        /// Log file.
        path: PathBuf,
        /// 1-based line number.
        line: usize,
        /// What was wrong.
        msg: String,
    },
    /// Event could not be serialised.
    #[error("serialise event: {0}")]
    Serialise(#[from] serde_json::Error),
    /// SQLite index failure.
    #[error("index error: {0}")]
    Index(#[from] rusqlite::Error),
    /// No such session.
    #[error("session not found: {0}")]
    NotFound(String),
    /// Session id already used.
    #[error("session already exists: {0}")]
    AlreadyExists(String),
    /// Session id would escape the store directory.
    #[error("invalid session id: {0:?}")]
    InvalidId(String),
    /// Fork target turn is not in the log.
    #[error("turn not found: {0}")]
    TurnNotFound(String),
    /// Fork target turn has no `turn/end`; only closed turns are fork points.
    #[error("turn still open: {0}")]
    TurnOpen(String),
}

/// Crate result alias.
pub type Result<T, E = SessionError> = std::result::Result<T, E>;
