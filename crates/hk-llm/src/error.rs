//! Error type shared by all providers.

use std::time::Duration;

/// Result alias for provider operations.
pub type Result<T> = std::result::Result<T, LlmError>;

/// Provider failure. Variants carry enough info for the router to decide
/// retry vs. fallback vs. surface-to-user (see [`LlmError::is_retryable`]).
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// Transport failure (DNS, TLS, connection reset, timeout).
    #[error("network error: {0}")]
    Network(String),
    /// Provider returned a non-success HTTP status.
    #[error("http {status}: {message}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Provider error message, already scrubbed of credentials.
        message: String,
        /// Server-requested wait before retrying (`Retry-After`), if any.
        retry_after: Option<Duration>,
    },
    /// Missing or rejected credentials.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// Prompt exceeds the model context window.
    #[error("context length exceeded: {0}")]
    ContextLength(String),
    /// Malformed or unexpected data on the wire.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// Request rejected before sending (bad model id, unsupported feature).
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The request was cancelled via its `CancellationToken`.
    #[error("cancelled")]
    Cancelled,
}

impl LlmError {
    /// Whether retrying the same request with backoff may succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Network(_) => true,
            Self::Http { status, .. } => matches!(*status, 408 | 409 | 425 | 429 | 500..=599),
            _ => false,
        }
    }
}
