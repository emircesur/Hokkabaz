//! Static bearer tokens with ordered scopes (`read` < `run` < `admin`).
//!
//! Tokens are random, generated on first start and kept behind an `hk_store::SecretStore`
//! handle; the `Store` only records the secret names. Values never reach logs: `Auth`'s Debug
//! is redacted and nothing here formats a token.

use std::fmt;

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::HeaderMap;
use axum::http::request::Parts;
use hk_store::{Secret, SecretStore, Store};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::api::AppState;
use crate::error::ApiError;

/// What a token may do. Higher scopes include lower ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// List/get sessions, models, WS event stream.
    Read,
    /// Read + submit / inject / cancel / approve.
    Run,
    /// Run + instance administration (reserved for later routes).
    Admin,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Read, Scope::Run, Scope::Admin];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Run => "run",
            Self::Admin => "admin",
        }
    }

    fn secret_name(self) -> String {
        format!("server.token.{}", self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("store: {0}")]
    Store(#[from] hk_store::StoreError),
    #[error("random source unavailable: {0}")]
    Random(String),
}

/// Token table. Cheap to clone.
#[derive(Clone, Default)]
pub struct Auth {
    tokens: Vec<(Scope, Secret)>,
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth")
            .field("scopes", &self.tokens.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>())
            .finish()
    }
}

impl Auth {
    /// No tokens: every protected route answers 401.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a token with `scope`. Tests and embedders; the daemon uses [`Auth::load_or_create`].
    #[must_use]
    pub fn with_token(mut self, scope: Scope, token: impl Into<String>) -> Self {
        self.tokens.push((scope, Secret::new(token)));
        self
    }

    /// Load one token per scope from `secrets`, creating (and registering in `store`) the
    /// missing ones.
    pub async fn load_or_create(store: &Store, secrets: &dyn SecretStore) -> Result<Self, AuthError> {
        let mut auth = Self::new();
        for scope in Scope::ALL {
            let name = scope.secret_name();
            let token = match store.get_secret(secrets, &name)? {
                Some(t) => t,
                None => {
                    let t = Secret::new(generate_token()?);
                    store.put_secret(secrets, &name, t.clone()).await?;
                    t
                }
            };
            auth.tokens.push((scope, token));
        }
        Ok(auth)
    }

    /// Token for `scope`, for the explicit "print my token" CLI path.
    pub fn token(&self, scope: Scope) -> Option<&Secret> {
        self.tokens.iter().find(|(s, _)| *s == scope).map(|(_, t)| t)
    }

    /// Highest scope `presented` grants. Compares against every entry without early exit so
    /// timing doesn't reveal which token (or how much of it) matched.
    pub fn authenticate(&self, presented: &str) -> Option<Scope> {
        let mut best: Option<Scope> = None;
        for (scope, token) in &self.tokens {
            if bool::from(token.expose().as_bytes().ct_eq(presented.as_bytes())) {
                best = best.max(Some(*scope));
            }
        }
        best
    }
}

fn generate_token() -> Result<String, AuthError> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| AuthError::Random(e.to_string()))?;
    let mut s = String::with_capacity(4 + 64);
    s.push_str("hkt_");
    for b in buf {
        s.push_str(&format!("{b:02x}"));
    }
    Ok(s)
}

/// Authenticated caller; handlers call [`Principal::require`] for their scope.
#[derive(Debug, Clone, Copy)]
pub struct Principal(pub Scope);

impl Principal {
    pub fn require(self, need: Scope) -> Result<(), ApiError> {
        if self.0 >= need { Ok(()) } else { Err(ApiError::Forbidden(need.as_str())) }
    }
}

/// `Authorization: Bearer <token>` value, if well formed.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty()).then_some(token.trim())
}

impl FromRequestParts<AppState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = bearer(&parts.headers).ok_or(ApiError::Unauthorized)?;
        state.auth.authenticate(token).map(Principal).ok_or(ApiError::Unauthorized)
    }
}

#[cfg(test)]
mod tests {
    use hk_store::MemorySecretStore;

    use super::*;

    #[test]
    fn highest_matching_scope() {
        let a = Auth::new().with_token(Scope::Read, "r").with_token(Scope::Admin, "a");
        assert_eq!(a.authenticate("r"), Some(Scope::Read));
        assert_eq!(a.authenticate("a"), Some(Scope::Admin));
        assert_eq!(a.authenticate("x"), None);
        assert_eq!(a.authenticate(""), None);
    }

    #[test]
    fn debug_hides_tokens() {
        let a = Auth::new().with_token(Scope::Run, "supersecret");
        assert!(!format!("{a:?}").contains("supersecret"));
    }

    #[tokio::test]
    async fn tokens_persist_across_loads() {
        let store = Store::open_in_memory().await.unwrap();
        let be = MemorySecretStore::default();
        let a = Auth::load_or_create(&store, &be).await.unwrap();
        let b = Auth::load_or_create(&store, &be).await.unwrap();
        for s in Scope::ALL {
            assert_eq!(a.token(s), b.token(s));
            assert!(a.token(s).unwrap().expose().starts_with("hkt_"));
        }
        assert_ne!(a.token(Scope::Read), a.token(Scope::Admin));
        assert_eq!(store.list_secret_names().await.unwrap().len(), 3);
    }
}
