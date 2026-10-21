//! API error type: one JSON shape for every failure.

/// Failure returned by handlers and runtimes. `code` strings are part of the wire contract.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ApiError {
    /// Missing or invalid bearer token.
    #[error("missing or invalid bearer token")]
    Unauthorized,
    /// Valid token without the required scope.
    #[error("token lacks required scope `{0}`")]
    Forbidden(&'static str),
    /// Unknown session / approval.
    #[error("{0}")]
    NotFound(String),
    /// Malformed or semantically invalid request.
    #[error("{0}")]
    BadRequest(String),
    /// Request is valid but clashes with current state.
    #[error("{0}")]
    Conflict(String),
    /// Server-side failure; the message must not carry secrets.
    #[error("{0}")]
    Internal(String),
}

impl ApiError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::NotFound(_) => "not_found",
            Self::BadRequest(_) => "bad_request",
            Self::Conflict(_) => "conflict",
            Self::Internal(_) => "internal",
        }
    }
}

/// HTTP mapping; only the axum surface needs it (the in-process runtime does not).
#[cfg(feature = "server")]
mod http {
    use axum::Json;
    use axum::http::{HeaderValue, StatusCode, header};
    use axum::response::{IntoResponse, Response};
    use serde_json::json;

    use super::ApiError;

    impl ApiError {
        pub fn status(&self) -> StatusCode {
            match self {
                Self::Unauthorized => StatusCode::UNAUTHORIZED,
                Self::Forbidden(_) => StatusCode::FORBIDDEN,
                Self::NotFound(_) => StatusCode::NOT_FOUND,
                Self::BadRequest(_) => StatusCode::BAD_REQUEST,
                Self::Conflict(_) => StatusCode::CONFLICT,
                Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            }
        }
    }

    impl IntoResponse for ApiError {
        fn into_response(self) -> Response {
            let body = Json(json!({ "error": { "code": self.code(), "message": self.to_string() } }));
            let mut res = (self.status(), body).into_response();
            if matches!(self, Self::Unauthorized) {
                res.headers_mut()
                    .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            res
        }
    }
}

impl From<hk_session::SessionError> for ApiError {
    fn from(e: hk_session::SessionError) -> Self {
        use hk_session::SessionError as E;
        match e {
            E::InvalidId(id) => Self::BadRequest(format!("invalid session id `{id}`")),
            E::NotFound(id) => Self::NotFound(format!("session `{id}` not found")),
            other => Self::Internal(other.to_string()),
        }
    }
}
