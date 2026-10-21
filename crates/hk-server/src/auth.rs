//! Static bearer tokens with scopes.
//!
//! The original tiers (`read` < `run` < `admin`) are kept; the n8n-shaped surface adds
//! fine-grained scopes (`workflow:read`, `workflow:write`, `execution:read`,
//! `credential:write`, `webhook:invoke`). A token holds a set of scopes; [`Scope::implies`]
//! says which needs a given scope satisfies. Tokens are random, generated on first start and
//! kept behind an `hk_store::SecretStore` handle; the `Store` only records the secret names
//! (and, for custom tokens, their scope lists in settings). Values never reach logs: `Auth`'s
//! Debug is redacted and nothing here formats a token.
//!
//! n8n clients send `X-N8N-API-KEY`; that header is accepted wherever `Authorization: Bearer`
//! is.

use std::fmt;

use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::Json;
use axum::http::header::{AUTHORIZATION, RETRY_AFTER};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use hk_store::{Secret, SecretStore, Store};
use serde_json::json;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::api::AppState;
use crate::error::ApiError;
use crate::ratelimit::Throttled;

/// What a token may do.
///
/// Implication: `admin` => everything; `run` => `read`, `webhook:invoke` and what `read`
/// gives; `read` => `workflow:read`, `execution:read`; `workflow:write` => `workflow:read`.
/// Writing workflows or credentials needs `admin` or the explicit scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    /// List/get sessions, models, WS event stream.
    #[serde(rename = "read")]
    Read,
    /// Read + submit / inject / cancel / approve.
    #[serde(rename = "run")]
    Run,
    /// Everything, including instance administration.
    #[serde(rename = "admin")]
    Admin,
    #[serde(rename = "workflow:read")]
    WorkflowRead,
    #[serde(rename = "workflow:write")]
    WorkflowWrite,
    #[serde(rename = "execution:read")]
    ExecutionRead,
    #[serde(rename = "credential:write")]
    CredentialWrite,
    /// Call webhooks configured with bearer auth.
    #[serde(rename = "webhook:invoke")]
    WebhookInvoke,
}

impl Scope {
    /// The three tiers that always get a generated token.
    pub const TIERS: [Scope; 3] = [Scope::Read, Scope::Run, Scope::Admin];
    pub const ALL: [Scope; 8] = [
        Scope::Read,
        Scope::Run,
        Scope::Admin,
        Scope::WorkflowRead,
        Scope::WorkflowWrite,
        Scope::ExecutionRead,
        Scope::CredentialWrite,
        Scope::WebhookInvoke,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Run => "run",
            Self::Admin => "admin",
            Self::WorkflowRead => "workflow:read",
            Self::WorkflowWrite => "workflow:write",
            Self::ExecutionRead => "execution:read",
            Self::CredentialWrite => "credential:write",
            Self::WebhookInvoke => "webhook:invoke",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.as_str() == s)
    }

    /// Does holding `self` satisfy a requirement for `need`?
    pub fn implies(self, need: Scope) -> bool {
        use Scope::*;
        match self {
            Admin => true,
            Run => matches!(need, Read | Run | WorkflowRead | ExecutionRead | WebhookInvoke),
            Read => matches!(need, Read | WorkflowRead | ExecutionRead),
            WorkflowWrite => matches!(need, WorkflowWrite | WorkflowRead),
            other => other == need,
        }
    }

    fn bit(self) -> u16 {
        1 << Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    fn tier_secret_name(self) -> String {
        format!("server.token.{}", self.as_str())
    }
}

/// Set of scopes a token holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Grants(u16);

impl Grants {
    pub fn of(scopes: &[Scope]) -> Self {
        Self(scopes.iter().fold(0, |a, s| a | s.bit()))
    }

    pub fn allows(self, need: Scope) -> bool {
        Scope::ALL.iter().any(|s| self.0 & s.bit() != 0 && s.implies(need))
    }

    fn union(self, o: Self) -> Self {
        Self(self.0 | o.0)
    }

    fn names(self) -> Vec<&'static str> {
        Scope::ALL.iter().filter(|s| self.0 & s.bit() != 0).map(|s| s.as_str()).collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("store: {0}")]
    Store(#[from] hk_store::StoreError),
    #[error("random source unavailable: {0}")]
    Random(String),
    #[error("token name must be 1-32 chars of [a-z0-9_-]")]
    BadName,
    #[error("a token with that name exists")]
    Exists,
    #[error("scopes must not be empty")]
    NoScopes,
    #[error("bootstrap token must be at least 24 characters without whitespace")]
    WeakToken,
    #[error("bad stored token table: {0}")]
    Corrupt(String),
}

const CUSTOM_TABLE: &str = "server.tokens";

#[derive(Clone)]
struct Entry {
    grants: Grants,
    token: Secret,
}

/// Token table. Cheap to clone.
#[derive(Clone, Default)]
pub struct Auth {
    /// Index = rate-limit key (see [`Principal::token_id`]).
    tokens: Vec<Entry>,
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth")
            .field("scopes", &self.tokens.iter().map(|t| t.grants.names()).collect::<Vec<_>>())
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
    pub fn with_token(self, scope: Scope, token: impl Into<String>) -> Self {
        self.with_scoped_token(&[scope], token)
    }

    /// Add a token holding several scopes.
    #[must_use]
    pub fn with_scoped_token(mut self, scopes: &[Scope], token: impl Into<String>) -> Self {
        self.tokens.push(Entry { grants: Grants::of(scopes), token: Secret::new(token) });
        self
    }

    /// Load one token per tier from `secrets`, creating (and registering in `store`) the
    /// missing ones, then any custom scoped tokens recorded in settings.
    pub async fn load_or_create(store: &Store, secrets: &dyn SecretStore) -> Result<Self, AuthError> {
        let mut auth = Self::new();
        for scope in Scope::TIERS {
            let name = scope.tier_secret_name();
            let token = match store.get_secret(secrets, &name)? {
                Some(t) => t,
                None => {
                    let t = Secret::new(generate_token()?);
                    store.put_secret(secrets, &name, t.clone()).await?;
                    t
                }
            };
            auth.tokens.push(Entry { grants: Grants::of(&[scope]), token });
        }
        for (name, scopes) in read_custom(store).await? {
            if let Some(token) = store.get_secret(secrets, &custom_secret_name(&name))? {
                auth.tokens.push(Entry { grants: Grants::of(&scopes), token });
            }
        }
        Ok(auth)
    }

    /// Pin the stored token of tier `scope` to an operator-supplied value (container secrets,
    /// provisioning). Run before [`Auth::load_or_create`]. Returns whether it changed anything.
    /// Rejects values that are too short to be unguessable.
    pub async fn bootstrap_tier_token(
        store: &Store,
        secrets: &dyn SecretStore,
        scope: Scope,
        token: &str,
    ) -> Result<bool, AuthError> {
        let token = token.trim();
        if token.len() < 24 || token.chars().any(char::is_whitespace) {
            return Err(AuthError::WeakToken);
        }
        let name = scope.tier_secret_name();
        if store.get_secret(secrets, &name)?.is_some_and(|t| t.expose() == token) {
            return Ok(false);
        }
        store.put_secret(secrets, &name, Secret::new(token)).await?;
        Ok(true)
    }

    /// Mint and persist a named token with `scopes`. The returned secret is the only time the
    /// caller sees it; print it once.
    pub async fn create_scoped_token(
        &mut self,
        store: &Store,
        secrets: &dyn SecretStore,
        name: &str,
        scopes: &[Scope],
    ) -> Result<Secret, AuthError> {
        if name.is_empty()
            || name.len() > 32
            || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        {
            return Err(AuthError::BadName);
        }
        if scopes.is_empty() {
            return Err(AuthError::NoScopes);
        }
        let mut table = read_custom(store).await?;
        if table.iter().any(|(n, _)| n == name) {
            return Err(AuthError::Exists);
        }
        let token = Secret::new(generate_token()?);
        store.put_secret(secrets, &custom_secret_name(name), token.clone()).await?;
        table.push((name.to_owned(), scopes.to_vec()));
        let json = serde_json::to_string(&table).map_err(|e| AuthError::Corrupt(e.to_string()))?;
        store.set_setting(CUSTOM_TABLE, &json).await?;
        self.tokens.push(Entry { grants: Grants::of(scopes), token: token.clone() });
        Ok(token)
    }

    /// Token for the tier `scope`, for the explicit "print my token" CLI path.
    pub fn token(&self, scope: Scope) -> Option<&Secret> {
        let g = Grants::of(&[scope]);
        self.tokens.iter().find(|t| t.grants == g).map(|t| &t.token)
    }

    /// Principal `presented` authenticates as. Compares against every entry without early exit
    /// so timing doesn't reveal which token (or how much of it) matched.
    pub fn authenticate(&self, presented: &str) -> Option<Principal> {
        let mut hit: Option<Principal> = None;
        for (i, t) in self.tokens.iter().enumerate() {
            if bool::from(t.token.expose().as_bytes().ct_eq(presented.as_bytes())) {
                hit = Some(match hit {
                    None => Principal { token_id: i, grants: t.grants },
                    Some(p) => Principal { token_id: p.token_id, grants: p.grants.union(t.grants) },
                });
            }
        }
        hit
    }
}

fn custom_secret_name(name: &str) -> String {
    format!("server.token.custom.{name}")
}

async fn read_custom(store: &Store) -> Result<Vec<(String, Vec<Scope>)>, AuthError> {
    match store.get_setting(CUSTOM_TABLE).await? {
        None => Ok(Vec::new()),
        Some(j) => serde_json::from_str(&j).map_err(|e| AuthError::Corrupt(e.to_string())),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Principal {
    /// Stable per-process token index; the rate-limit key.
    pub token_id: usize,
    pub grants: Grants,
}

impl Principal {
    /// A principal holding exactly `scope` (tests, in-process callers).
    pub fn of(scope: Scope) -> Self {
        Self { token_id: usize::MAX, grants: Grants::of(&[scope]) }
    }

    pub fn allows(self, need: Scope) -> bool {
        self.grants.allows(need)
    }

    pub fn require(self, need: Scope) -> Result<(), ApiError> {
        if self.allows(need) { Ok(()) } else { Err(ApiError::Forbidden(need.as_str())) }
    }
}

/// Token from `Authorization: Bearer <token>` or `X-N8N-API-KEY`, if well formed.
pub(crate) fn token_from(headers: &HeaderMap) -> Option<&str> {
    if let Some(t) = bearer(headers) {
        return Some(t);
    }
    let k = headers.get("x-n8n-api-key")?.to_str().ok()?.trim();
    (!k.is_empty()).then_some(k)
}

/// `Authorization: Bearer <token>` value, if well formed.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty()).then_some(token.trim())
}

/// Why a request was turned away before reaching a handler. Kept out of [`ApiError`] so that
/// enum (matched exhaustively by hk-ffi) stays unchanged.
#[derive(Debug, Clone)]
pub enum Reject {
    Api(ApiError),
    Throttled(Throttled),
}

impl From<ApiError> for Reject {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<Throttled> for Reject {
    fn from(t: Throttled) -> Self {
        Self::Throttled(t)
    }
}

impl IntoResponse for Reject {
    fn into_response(self) -> Response {
        match self {
            Self::Api(e) => e.into_response(),
            Self::Throttled(Throttled(secs)) => {
                let body = Json(json!({ "error": { "code": "rate_limited", "message": format!("rate limit exceeded, retry in {secs}s") } }));
                let mut res = (StatusCode::TOO_MANY_REQUESTS, body).into_response();
                res.headers_mut().insert(RETRY_AFTER, HeaderValue::from(secs));
                res
            }
        }
    }
}

/// Authenticate `token` and charge it one rate-limit unit.
pub(crate) fn authorize(state: &AppState, token: Option<&str>) -> Result<Principal, Reject> {
    let p = state.auth.authenticate(token.ok_or(ApiError::Unauthorized)?).ok_or(ApiError::Unauthorized)?;
    state.limiter.check(p.token_id)?;
    Ok(p)
}

impl FromRequestParts<AppState> for Principal {
    type Rejection = Reject;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Reject> {
        authorize(state, token_from(&parts.headers))
    }
}

#[cfg(test)]
mod tests {
    use hk_store::MemorySecretStore;

    use super::*;

    fn can(a: &Auth, tok: &str, need: Scope) -> bool {
        a.authenticate(tok).is_some_and(|p| p.allows(need))
    }

    #[test]
    fn tiers_are_ordered() {
        let a = Auth::new().with_token(Scope::Read, "r").with_token(Scope::Admin, "a");
        assert!(can(&a, "r", Scope::Read));
        assert!(!can(&a, "r", Scope::Run));
        assert!(can(&a, "a", Scope::Run));
        assert!(can(&a, "a", Scope::CredentialWrite));
        assert!(a.authenticate("x").is_none());
        assert!(a.authenticate("").is_none());
    }

    #[test]
    fn fine_grained_scopes_do_not_leak() {
        let a = Auth::new()
            .with_token(Scope::WorkflowWrite, "ww")
            .with_token(Scope::CredentialWrite, "cw")
            .with_token(Scope::Run, "run")
            .with_scoped_token(&[Scope::ExecutionRead, Scope::WebhookInvoke], "multi");
        assert!(can(&a, "ww", Scope::WorkflowRead), "write implies read");
        assert!(!can(&a, "ww", Scope::ExecutionRead));
        assert!(!can(&a, "ww", Scope::Read));
        assert!(!can(&a, "cw", Scope::WorkflowRead));
        assert!(can(&a, "run", Scope::WebhookInvoke));
        assert!(can(&a, "run", Scope::WorkflowRead));
        assert!(!can(&a, "run", Scope::WorkflowWrite), "run must not write workflows");
        assert!(!can(&a, "run", Scope::CredentialWrite));
        assert!(can(&a, "multi", Scope::ExecutionRead) && can(&a, "multi", Scope::WebhookInvoke));
        assert!(!can(&a, "multi", Scope::WorkflowRead));
    }

    #[test]
    fn scope_names_roundtrip() {
        for s in Scope::ALL {
            assert_eq!(Scope::parse(s.as_str()), Some(s));
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.as_str()));
        }
        assert_eq!(Scope::parse("nope"), None);
    }

    #[test]
    fn n8n_api_key_header_is_accepted() {
        let mut h = HeaderMap::new();
        h.insert("x-n8n-api-key", "k1".parse().unwrap());
        assert_eq!(token_from(&h), Some("k1"));
        h.insert(AUTHORIZATION, "Bearer b1".parse().unwrap());
        assert_eq!(token_from(&h), Some("b1"), "bearer wins");
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
        for s in Scope::TIERS {
            assert_eq!(a.token(s), b.token(s));
            assert!(a.token(s).unwrap().expose().starts_with("hkt_"));
        }
        assert_ne!(a.token(Scope::Read), a.token(Scope::Admin));
        assert_eq!(store.list_secret_names().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn custom_tokens_persist_with_scopes() {
        let store = Store::open_in_memory().await.unwrap();
        let be = MemorySecretStore::default();
        let mut a = Auth::load_or_create(&store, &be).await.unwrap();
        let t = a.create_scoped_token(&store, &be, "ci", &[Scope::WorkflowRead]).await.unwrap();
        assert!(matches!(
            a.create_scoped_token(&store, &be, "ci", &[Scope::Read]).await,
            Err(AuthError::Exists)
        ));
        assert!(matches!(
            a.create_scoped_token(&store, &be, "Bad Name", &[Scope::Read]).await,
            Err(AuthError::BadName)
        ));
        let b = Auth::load_or_create(&store, &be).await.unwrap();
        let p = b.authenticate(t.expose()).expect("custom token reloaded");
        assert!(p.allows(Scope::WorkflowRead) && !p.allows(Scope::Read));
    }
}
