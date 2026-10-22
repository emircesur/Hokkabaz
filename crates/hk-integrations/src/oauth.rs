//! OAuth2 refresh-token credentials (Google, Microsoft, ...).
//!
//! The user supplies `clientId`, `clientSecret` and a long-lived `refreshToken` (obtained once with
//! the provider's consent screen or playground); at request time the short-lived access token is
//! minted from `tokenUrl` and cached in memory until shortly before it expires. The minted token
//! reaches the credential `authenticate` template as `$credentials.accessToken`; it is never
//! stored, logged or put in an error.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::template::display;

struct Cached {
    token: String,
    until: Instant,
}

static CACHE: Mutex<Option<HashMap<u64, Cached>>> = Mutex::new(None);

fn key(token_url: &str, client_id: &str, refresh: &str) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    (token_url, client_id, refresh).hash(&mut h);
    h.finish()
}

fn field(data: &Map<String, Value>, k: &str) -> String {
    data.get(k).map(display).unwrap_or_default()
}

/// Is this credential data an OAuth2 refresh-token credential?
pub fn applies(data: &Map<String, Value>) -> bool {
    !field(data, "refreshToken").is_empty() && !field(data, "tokenUrl").is_empty()
}

/// An access token, plus the refresh token the provider rotated to (when it did).
pub struct Minted {
    pub token: String,
    pub new_refresh: Option<String>,
}

/// X (Twitter) wants confidential clients to authenticate with HTTP Basic, not form fields.
pub(crate) fn basic_client_auth(token_url: &str) -> bool {
    token_url.contains("api.x.com") || token_url.contains("api.twitter.com")
}

/// Access token for `data` (from cache or the token endpoint).
pub async fn access_token(
    client: &reqwest::Client,
    data: &Map<String, Value>,
) -> Result<Minted, String> {
    // `{{$credentials.tenant}}` in the URL (Microsoft) renders from the same credential.
    let url = crate::template::render(&field(data, "tokenUrl"), data).map_err(|e| e.to_string())?;
    let id = field(data, "clientId");
    let refresh = field(data, "refreshToken");
    let k = key(&url, &id, &refresh);
    if let Some(c) = CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|m| m.get(&k))
        && c.until > Instant::now()
    {
        return Ok(Minted {
            token: c.token.clone(),
            new_refresh: None,
        });
    }
    let secret = field(data, "clientSecret");
    let basic = basic_client_auth(&url) && !secret.is_empty();
    let mut form = vec![
        ("grant_type", "refresh_token".to_owned()),
        ("refresh_token", refresh.clone()),
    ];
    if !basic {
        form.push(("client_id", id.clone()));
        if !secret.is_empty() {
            form.push(("client_secret", secret.clone()));
        }
    }
    let scope = field(data, "scope");
    if !scope.is_empty() {
        form.push(("scope", scope));
    }
    let mut rb = client.post(&url).header("Accept", "application/json");
    if basic {
        rb = rb.basic_auth(&id, Some(&secret));
    }
    let resp = rb
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("token endpoint unreachable: {}", e.without_url()))?;
    let status = resp.status();
    let body: Value = resp
        .text()
        .await
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let Some(token) = body.get("access_token").and_then(Value::as_str) else {
        // `error` / `error_description` are the provider's words about the grant, never our secrets.
        let why = body
            .get("error_description")
            .or_else(|| body.get("error"))
            .map(display)
            .unwrap_or_default();
        return Err(format!(
            "token endpoint refused the refresh token ({status}){}{}",
            if why.is_empty() { "" } else { ": " },
            why.chars().take(200).collect::<String>()
        ));
    };
    let ttl = body
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(3000);
    // Refresh a minute early so a request in flight never carries an expired token.
    let until = Instant::now() + Duration::from_secs(ttl.saturating_sub(60).max(30));
    CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .insert(
            k,
            Cached {
                token: token.to_owned(),
                until,
            },
        );
    // Providers that rotate refresh tokens (Microsoft, X) return the replacement here.
    let new_refresh = body
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty() && *r != refresh)
        .map(str::to_owned);
    Ok(Minted {
        token: token.to_owned(),
        new_refresh,
    })
}
