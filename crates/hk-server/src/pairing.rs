//! Device pairing: scan a QR on the desktop/terminal, get a scoped API token on the phone.
//!
//! A pairing token is short-lived, single-use and stateless to mint: `hkp_<scope>.<exp>.<nonce>.<mac>`
//! with `mac = HMAC-SHA256(K, "<scope>.<exp>.<nonce>")` and `K` derived from the instance's
//! admin token. So `hokkabazd pair` (a separate process) can mint tokens the running daemon
//! accepts, without talking to it. Single use is enforced in memory for the token's lifetime
//! (a restart inside the TTL window forgets used nonces; the window is minutes).
//!
//! The pairing token is not an API token: it only works on `POST /api/v1/pairing/exchange`,
//! which swaps it for the instance's token of the requested tier (`read` / `run` / `admin`).
//! Treat the QR like a password for its TTL; serve over TLS beyond a trusted LAN.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::Json;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;

use crate::api::AppState;
use crate::auth::{Auth, Principal, Scope};
use crate::error::ApiError;

type HmacSha256 = Hmac<Sha256>;

pub const DEFAULT_TTL: Duration = Duration::from_secs(300);
const PREFIX: &str = "hkp_";
const KDF_LABEL: &[u8] = b"hokkabaz/pairing/v1";

#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    #[error("pairing needs the admin token to exist")]
    NoAdminToken,
    #[error("pairing scope must be read, run or admin")]
    BadScope,
    #[error("random source unavailable: {0}")]
    Random(String),
    #[error("invalid or expired pairing token")]
    Invalid,
    #[error("qr: {0}")]
    Qr(String),
}

/// A freshly minted pairing token and what to show for it.
#[derive(Debug, Clone)]
pub struct Minted {
    /// The short-lived pairing token (not an API token).
    pub token: String,
    pub scope: Scope,
    /// Unix seconds.
    pub expires_at: u64,
    /// Instance base URL the app should talk to.
    pub url: String,
    /// What the QR encodes: `hokkabaz://pair?url=<base>&token=<pairing token>`.
    pub payload: String,
}

impl Minted {
    pub fn qr_svg(&self) -> Result<String, PairingError> {
        qr_svg(&self.payload)
    }

    /// Unicode half-block rendering, light-on-dark (scans on a dark terminal).
    pub fn qr_terminal(&self) -> Result<String, PairingError> {
        qr_terminal(&self.payload)
    }
}

/// Mints and redeems pairing tokens. Cheap to share behind an `Arc`.
pub struct Pairing {
    key: [u8; 32],
    public_url: String,
    ttl: Duration,
    /// nonce -> expiry (unix s); entries are dropped once expired.
    used: Mutex<HashMap<String, u64>>,
}

impl std::fmt::Debug for Pairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pairing").field("public_url", &self.public_url).field("ttl", &self.ttl).finish_non_exhaustive()
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

fn mac(key: &[u8]) -> HmacSha256 {
    HmacSha256::new_from_slice(key).expect("hmac accepts any key length")
}

impl Pairing {
    /// Key derived from `auth`'s admin token; `None` when the instance has none.
    pub fn new(auth: &Auth, public_url: impl Into<String>) -> Result<Self, PairingError> {
        let admin = auth.token(Scope::Admin).ok_or(PairingError::NoAdminToken)?;
        let mut m = mac(admin.expose().as_bytes());
        m.update(KDF_LABEL);
        let key: [u8; 32] = m.finalize().into_bytes().into();
        Ok(Self {
            key,
            public_url: public_url.into().trim_end_matches('/').to_owned(),
            ttl: DEFAULT_TTL,
            used: Mutex::new(HashMap::new()),
        })
    }

    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn public_url(&self) -> &str {
        &self.public_url
    }

    pub fn mint(&self, scope: Scope) -> Result<Minted, PairingError> {
        self.mint_at(scope, unix_now())
    }

    pub fn mint_at(&self, scope: Scope, now: u64) -> Result<Minted, PairingError> {
        if !Scope::TIERS.contains(&scope) {
            return Err(PairingError::BadScope);
        }
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|e| PairingError::Random(e.to_string()))?;
        let exp = now + self.ttl.as_secs();
        let body = format!("{}.{exp}.{}", scope.as_str(), hex(&nonce));
        let mut m = mac(&self.key);
        m.update(body.as_bytes());
        let token = format!("{PREFIX}{body}.{}", hex(&m.finalize().into_bytes()));
        let url = self.public_url.clone();
        let enc_url: String = form_urlencoded::byte_serialize(url.as_bytes()).collect();
        let payload = format!("hokkabaz://pair?url={enc_url}&token={token}");
        Ok(Minted { token, scope, expires_at: exp, url, payload })
    }

    /// Check and burn `token`. Every failure is the same opaque error.
    pub fn redeem(&self, token: &str) -> Result<Scope, PairingError> {
        self.redeem_at(token, unix_now())
    }

    pub fn redeem_at(&self, token: &str, now: u64) -> Result<Scope, PairingError> {
        let rest = token.strip_prefix(PREFIX).ok_or(PairingError::Invalid)?;
        let mut parts = rest.split('.');
        let (Some(scope), Some(exp), Some(nonce), Some(sig), None) =
            (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(PairingError::Invalid);
        };
        let scope = Scope::parse(scope).filter(|s| Scope::TIERS.contains(s)).ok_or(PairingError::Invalid)?;
        let exp: u64 = exp.parse().map_err(|_| PairingError::Invalid)?;
        let sig = unhex(sig).ok_or(PairingError::Invalid)?;
        let mut m = mac(&self.key);
        m.update(format!("{}.{exp}.{nonce}", scope.as_str()).as_bytes());
        // verify_slice compares in constant time
        m.verify_slice(&sig).map_err(|_| PairingError::Invalid)?;
        if exp <= now {
            return Err(PairingError::Invalid);
        }
        let mut used = self.used.lock().unwrap_or_else(PoisonError::into_inner);
        used.retain(|_, e| *e > now);
        if used.insert(nonce.to_owned(), exp).is_some() {
            return Err(PairingError::Invalid);
        }
        Ok(scope)
    }
}

pub fn qr_svg(payload: &str) -> Result<String, PairingError> {
    let code = qrcode::QrCode::new(payload.as_bytes()).map_err(|e| PairingError::Qr(e.to_string()))?;
    Ok(code.render::<qrcode::render::svg::Color>().min_dimensions(256, 256).quiet_zone(true).build())
}

pub fn qr_terminal(payload: &str) -> Result<String, PairingError> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::new(payload.as_bytes()).map_err(|e| PairingError::Qr(e.to_string()))?;
    Ok(code.render::<Dense1x2>().dark_color(Dense1x2::Light).light_color(Dense1x2::Dark).quiet_zone(true).build())
}

// ------------------------------------------------------------------ routes

#[derive(Deserialize)]
pub(crate) struct IssueQuery {
    scope: Option<String>,
}

/// `GET /api/v1/pairing?scope=run|read|admin`: needs `run` (or `admin`); the requested scope
/// cannot exceed the caller's.
pub(crate) async fn issue(
    p: Principal,
    State(s): State<AppState>,
    Query(q): Query<IssueQuery>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Run)?;
    let scope = match q.scope.as_deref() {
        None => Scope::Run,
        Some(n) => Scope::parse(n)
            .filter(|s| Scope::TIERS.contains(s))
            .ok_or_else(|| ApiError::BadRequest("`scope` must be read, run or admin".into()))?,
    };
    p.require(scope)?;
    let pairing = s.pairing.as_ref().ok_or_else(|| ApiError::NotFound("pairing is not enabled".into()))?;
    let m = pairing.mint(scope).map_err(|e| ApiError::Internal(e.to_string()))?;
    let qr_svg = m.qr_svg().map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "url": m.url,
        "token": m.token,
        "scope": m.scope.as_str(),
        "expires_at": m.expires_at,
        "payload": m.payload,
        "qr_svg": qr_svg,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExchangeBody {
    token: String,
}

/// `POST /api/v1/pairing/exchange {token}`: no bearer needed, the pairing token is the proof.
pub(crate) async fn exchange(
    State(s): State<AppState>,
    Json(b): Json<ExchangeBody>,
) -> Result<Json<Value>, ApiError> {
    let pairing = s.pairing.as_ref().ok_or_else(|| ApiError::NotFound("pairing is not enabled".into()))?;
    let scope = pairing.redeem(&b.token).map_err(|_| ApiError::Unauthorized)?;
    let token = s.auth.token(scope).ok_or_else(|| ApiError::Internal("instance has no token for that scope".into()))?;
    Ok(Json(json!({ "token": token.expose(), "scope": scope.as_str(), "url": pairing.public_url() })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, header};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::tests::Fake;
    use crate::{AppState, router};

    fn auth() -> Auth {
        Auth::new().with_token(Scope::Read, "r").with_token(Scope::Run, "u").with_token(Scope::Admin, "a")
    }

    #[test]
    fn mint_redeem_once_expiry_tamper() {
        let p = Pairing::new(&auth(), "http://h:1/").unwrap();
        assert_eq!(p.public_url(), "http://h:1");
        let m = p.mint_at(Scope::Run, 1000).unwrap();
        assert_eq!(m.expires_at, 1300);
        assert!(m.payload.starts_with("hokkabaz://pair?url=http%3A%2F%2Fh%3A1&token=hkp_run."));
        assert_eq!(p.redeem_at(&m.token, 1100).unwrap(), Scope::Run);
        assert!(p.redeem_at(&m.token, 1101).is_err(), "single use");

        let m = p.mint_at(Scope::Admin, 1000).unwrap();
        assert!(p.redeem_at(&m.token, 1300).is_err(), "expired");
        let m = p.mint_at(Scope::Read, 1000).unwrap();
        let forged = m.token.replacen("hkp_read", "hkp_admin", 1);
        assert!(p.redeem_at(&forged, 1100).is_err(), "scope bound to mac");
        assert!(p.redeem_at("hkp_run.1.2.zz", 1).is_err());
        assert!(p.mint_at(Scope::WorkflowRead, 1).is_err());

        // another instance (other admin token) rejects it; same admin token accepts (CLI vs daemon)
        let other = Pairing::new(&Auth::new().with_token(Scope::Admin, "b"), "x").unwrap();
        assert!(other.redeem_at(&m.token, 1100).is_err());
        let twin = Pairing::new(&auth(), "http://elsewhere").unwrap();
        assert_eq!(twin.redeem_at(&m.token, 1100).unwrap(), Scope::Read);
    }

    #[test]
    fn qr_renders() {
        let m = Pairing::new(&auth(), "http://h").unwrap().mint(Scope::Run).unwrap();
        assert!(m.qr_svg().unwrap().contains("<svg"));
        assert!(m.qr_terminal().unwrap().contains('\n'));
    }

    async fn call(app: &axum::Router, method: Method, uri: &str, tok: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(uri);
        if let Some(t) = tok {
            b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let req = match body {
            Some(v) => b.header(header::CONTENT_TYPE, "application/json").body(Body::from(v.to_string())),
            None => b.body(Body::empty()),
        }
        .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        let st = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn http_issue_and_exchange() {
        let a = auth();
        let state = AppState::new(Arc::new(Fake::default()), a.clone())
            .with_pairing(Pairing::new(&a, "http://box:7878").unwrap());
        let app = router(state);

        assert_eq!(call(&app, Method::GET, "/api/v1/pairing", None, None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(call(&app, Method::GET, "/api/v1/pairing", Some("r"), None).await.0, StatusCode::FORBIDDEN);
        // run token cannot mint an admin pairing
        assert_eq!(call(&app, Method::GET, "/api/v1/pairing?scope=admin", Some("u"), None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call(&app, Method::GET, "/api/v1/pairing?scope=bogus", Some("a"), None).await.0, StatusCode::BAD_REQUEST);

        let (st, v) = call(&app, Method::GET, "/api/v1/pairing", Some("u"), None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["url"], "http://box:7878");
        assert_eq!(v["scope"], "run");
        assert!(v["qr_svg"].as_str().unwrap().contains("<svg"));
        let tok = v["token"].as_str().unwrap().to_owned();
        assert!(!v.to_string().contains("\"u\""), "response must not carry the bearer");

        let (st, v) = call(&app, Method::POST, "/api/v1/pairing/exchange", None, Some(json!({"token": tok}))).await;
        assert_eq!((st, v["token"].as_str(), v["scope"].as_str()), (StatusCode::OK, Some("u"), Some("run")));
        let (st, _) = call(&app, Method::POST, "/api/v1/pairing/exchange", None, Some(json!({"token": tok}))).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "replay");
    }
}
