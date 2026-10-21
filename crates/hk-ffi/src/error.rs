//! Error type crossing the FFI boundary.
//!
//! Kept a struct + plain enum (not an enum with payloads) so the Dart side is a
//! simple class without `freezed`.

/// Coarse error class; the UI switches on this, shows `message` to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HkErrorKind {
    /// Bug or unexpected state inside the core.
    Internal,
    /// Dart side dropped the stream/receiver.
    Closed,
    /// Caller passed an invalid argument.
    InvalidArgument,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct HkError {
    pub kind: HkErrorKind,
    pub message: String,
}

impl HkError {
    pub fn new(kind: HkErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
