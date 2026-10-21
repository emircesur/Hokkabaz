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
    /// `core_init` has not run (or `core_shutdown` already did).
    NotInitialized,
    /// Unknown session / approval / provider.
    NotFound,
    /// Valid request that clashes with current state (e.g. duplicate provider id).
    Conflict,
    /// Provider or model misconfiguration (missing key, bad endpoint).
    Provider,
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

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(HkErrorKind::Internal, message)
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(HkErrorKind::InvalidArgument, message)
    }
}

impl From<hk_server::ApiError> for HkError {
    fn from(e: hk_server::ApiError) -> Self {
        use hk_server::ApiError as E;
        let kind = match &e {
            E::NotFound(_) => HkErrorKind::NotFound,
            E::BadRequest(_) => HkErrorKind::InvalidArgument,
            E::Conflict(_) => HkErrorKind::Conflict,
            E::Unauthorized | E::Forbidden(_) | E::Internal(_) => HkErrorKind::Internal,
        };
        Self::new(kind, e.to_string())
    }
}

impl From<hk_store::StoreError> for HkError {
    fn from(e: hk_store::StoreError) -> Self {
        Self::internal(format!("store: {e}"))
    }
}

impl From<hk_store::SecretError> for HkError {
    fn from(e: hk_store::SecretError) -> Self {
        Self::internal(e.to_string())
    }
}

impl From<serde_json::Error> for HkError {
    fn from(e: serde_json::Error) -> Self {
        Self::internal(format!("json: {e}"))
    }
}
