//! Error type for the whole crate.

use std::path::PathBuf;

/// Transcription failure.
#[derive(Debug, thiserror::Error)]
pub enum SttError {
    /// Local file problem (audio unreadable, temp dir, ...).
    #[error("io {path}: {source}")]
    Io {
        /// File involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// Connect/timeout/transport failure after all retries.
    #[error("network: {0}")]
    Network(String),
    /// Non-success HTTP status after all retries (body truncated).
    #[error("http {status}: {body}")]
    Http {
        /// Status code.
        status: u16,
        /// Truncated response body.
        body: String,
    },
    /// Response not understood.
    #[error("parse: {0}")]
    Parse(String),
    /// Audio exceeds the request size limit; chunk it first.
    #[error("audio is {size} bytes, limit {limit}; use ChunkedTranscriber")]
    TooLarge {
        /// Actual size.
        size: u64,
        /// Limit.
        limit: u64,
    },
    /// `key_ref` set but resolver returned nothing.
    #[error("no key for ref {0:?}")]
    MissingKey(String),
    /// Bad config.
    #[error("config: {0}")]
    Config(String),
    /// Provider not compiled in.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Audio decode/convert problem.
    #[error("audio: {0}")]
    Audio(String),
    /// [`crate::AudioSplitter`] failure.
    #[error("splitter: {0}")]
    Splitter(String),
}

/// Crate result.
pub type Result<T> = std::result::Result<T, SttError>;
