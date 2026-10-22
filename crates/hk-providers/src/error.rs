use hk_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum ProvidersError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Provider(String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("settings json: {0}")]
    Json(#[from] serde_json::Error),
}

impl ProvidersError {
    pub fn invalid(m: impl Into<String>) -> Self {
        Self::Invalid(m.into())
    }
}
