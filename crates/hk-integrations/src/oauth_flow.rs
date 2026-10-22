//! Browser sign-in for OAuth2 credentials: authorization-code flow with PKCE and a loopback
//! redirect (`http://127.0.0.1:53682/callback`), the way desktop OAuth clients work. The user
//! registers their own app with the provider (client id / secret), then signs in once; the refresh
//! token lands in the credential form, replacing the copy-paste step.
//!
//! Flow: [`begin`] binds the loopback listener and returns the URL to open; the UI opens it in the
//! browser and calls [`finish`], which resolves when the provider redirects back and the code has
//! been exchanged. [`cancel`] ends a flow the user abandoned.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util_lite::Cancel;

use crate::template::{display, render};

/// Preferred loopback port: providers that match redirect URIs exactly (Dropbox, X, LinkedIn,
/// Salesforce) need a fixed one registered in the app settings.
pub const PORT: u16 = 53682;
const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// A cancel flag without pulling another crate.
mod tokio_util_lite {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::sync::Notify;

    #[derive(Clone, Default)]
    pub struct Cancel(Arc<(AtomicBool, Notify)>);

    impl Cancel {
        pub fn cancel(&self) {
            self.0.0.store(true, Ordering::SeqCst);
            self.0.1.notify_waiters();
        }
        pub async fn cancelled(&self) {
            loop {
                let n = self.0.1.notified();
                if self.0.0.load(Ordering::SeqCst) {
                    return;
                }
                n.await;
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Begin {
    pub state: String,
    pub auth_url: String,
    pub redirect_uri: String,
}

/// What the provider handed back. Field names are the credential fields they fill.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Tokens {
    #[serde(rename = "refreshToken", skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(rename = "accessToken", skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// Salesforce says which org host to talk to.
    #[serde(rename = "instanceUrl", skip_serializing_if = "Option::is_none")]
    pub instance_url: Option<String>,
}

struct Pending {
    rx: Option<oneshot::Receiver<Result<Tokens, String>>>,
    cancel: Cancel,
}

static PENDING: Mutex<Option<HashMap<String, Pending>>> = Mutex::new(None);

fn rand_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    // a failing OS RNG would make state/verifier guessable: refuse rather than fall back
    getrandom::fill(&mut b).expect("OS random source");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Everything [`begin`] needs. `data` is the credential form (templates like
/// `{{$credentials.tenant}}` in the URLs render from it).
#[derive(Debug, Clone)]
pub struct Params {
    pub data: Map<String, Value>,
}

fn field(d: &Map<String, Value>, k: &str) -> String {
    d.get(k).map(display).unwrap_or_default().trim().to_owned()
}

pub async fn begin(p: Params) -> Result<Begin, String> {
    let d = &p.data;
    let client_id = field(d, "clientId");
    let auth_url = render(&field(d, "authUrl"), d).map_err(|e| e.to_string())?;
    let token_url = render(&field(d, "tokenUrl"), d).map_err(|e| e.to_string())?;
    if client_id.is_empty() {
        return Err("Enter the Client ID first".into());
    }
    if auth_url.is_empty() || token_url.is_empty() {
        return Err("This credential type has no sign-in page".into());
    }
    let listener = match TcpListener::bind(("127.0.0.1", PORT)).await {
        Ok(l) => l,
        Err(_) => TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("cannot listen for the sign-in redirect: {e}"))?,
    };
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let state = rand_hex(16);
    let verifier = rand_hex(32);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));

    let mut q = vec![
        ("response_type".to_owned(), "code".to_owned()),
        ("client_id".to_owned(), client_id.clone()),
        ("redirect_uri".to_owned(), redirect_uri.clone()),
        ("state".to_owned(), state.clone()),
        ("code_challenge".to_owned(), challenge),
        ("code_challenge_method".to_owned(), "S256".to_owned()),
    ];
    let scope = field(d, "scope");
    if !scope.is_empty() {
        q.push(("scope".into(), scope));
    }
    for kv in field(d, "authParams").split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        q.push((k.to_owned(), v.to_owned()));
    }
    let sep = if auth_url.contains('?') { '&' } else { '?' };
    let url = format!(
        "{auth_url}{sep}{}",
        q.iter()
            .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
            .collect::<Vec<_>>()
            .join("&")
    );

    let (tx, rx) = oneshot::channel();
    let cancel = Cancel::default();
    PENDING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .insert(
            state.clone(),
            Pending {
                rx: Some(rx),
                cancel: cancel.clone(),
            },
        );
    let (st, secret) = (state.clone(), field(d, "clientSecret"));
    let redirect = redirect_uri.clone();
    tokio::spawn(async move {
        let r = tokio::select! {
            () = cancel.cancelled() => Err("sign-in cancelled".to_owned()),
            r = tokio::time::timeout(TIMEOUT, serve(listener, &st)) => match r {
                Ok(r) => r,
                Err(_) => Err("sign-in timed out".to_owned()),
            },
        };
        let out = match r {
            Ok(code) => {
                exchange(&token_url, &client_id, &secret, &code, &redirect, &verifier).await
            }
            Err(e) => Err(e),
        };
        let _ = tx.send(out);
    });
    Ok(Begin {
        state,
        auth_url: url,
        redirect_uri,
    })
}

/// Wait for the sign-in `state` started by [`begin`] to complete.
pub async fn finish(state: &str) -> Result<Tokens, String> {
    let rx = PENDING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
        .and_then(|m| m.get_mut(state))
        .and_then(|p| p.rx.take())
        .ok_or_else(|| "no sign-in in progress".to_owned())?;
    let r = rx.await.unwrap_or_else(|_| Err("sign-in ended".into()));
    PENDING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_mut()
        .map(|m| m.remove(state));
    r
}

pub fn cancel(state: &str) {
    if let Some(p) = PENDING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|m| m.get(state))
    {
        p.cancel.cancel();
    }
}

const PAGE: &str = "<!doctype html><meta charset=utf-8><title>Hokkabaz</title>\
<body style=\"font-family:system-ui;margin:3rem;text-align:center\"><h2>Signed in</h2>\
<p>You can close this tab and go back to Hokkabaz.</p></body>";
const FAIL: &str = "<!doctype html><meta charset=utf-8><title>Hokkabaz</title>\
<body style=\"font-family:system-ui;margin:3rem;text-align:center\"><h2>Sign-in failed</h2>\
<p>Go back to Hokkabaz for the reason.</p></body>";

/// Accept connections until the provider's redirect with the right `state` arrives.
async fn serve(listener: TcpListener, state: &str) -> Result<String, String> {
    loop {
        let (mut sock, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 8192];
        let n = tokio::time::timeout(Duration::from_secs(10), sock.read(&mut buf))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
        let target = req
            .lines()
            .next()
            .and_then(|l| l.split(' ').nth(1))
            .unwrap_or("");
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if path != "/callback" {
            let _ = sock
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let params: HashMap<String, String> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (dec(k), dec(v)))
            .collect();
        let ok = params.get("state").map(String::as_str) == Some(state);
        let result = if !ok {
            Err("the sign-in redirect did not match this request".to_owned())
        } else if let Some(e) = params.get("error") {
            let why = params.get("error_description").cloned().unwrap_or_default();
            Err(format!("the provider refused: {e} {why}").trim().to_owned())
        } else {
            params
                .get("code")
                .cloned()
                .ok_or_else(|| "the redirect carried no code".to_owned())
        };
        let body = if result.is_ok() { PAGE } else { FAIL };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.shutdown().await;
        // a stray request with the wrong state must not end the flow
        if ok {
            return result;
        }
    }
}

async fn exchange(
    token_url: &str,
    client_id: &str,
    secret: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Tokens, String> {
    let basic = crate::oauth::basic_client_auth(token_url) && !secret.is_empty();
    let mut form = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", code.to_owned()),
        ("redirect_uri", redirect_uri.to_owned()),
        ("code_verifier", verifier.to_owned()),
    ];
    if !basic {
        form.push(("client_id", client_id.to_owned()));
        if !secret.is_empty() {
            form.push(("client_secret", secret.to_owned()));
        }
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.without_url().to_string())?;
    let mut rb = client.post(token_url).header("Accept", "application/json");
    if basic {
        rb = rb.basic_auth(client_id, Some(secret));
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
    let get = |k: &str| {
        body.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let (refresh_token, access_token) = (get("refresh_token"), get("access_token"));
    if refresh_token.is_none() && access_token.is_none() {
        let why = get("error_description")
            .or_else(|| get("error"))
            .unwrap_or_default();
        return Err(format!(
            "the provider did not issue tokens ({status}){}{}",
            if why.is_empty() { "" } else { ": " },
            why.chars().take(200).collect::<String>()
        ));
    }
    Ok(Tokens {
        refresh_token,
        access_token,
        instance_url: get("instance_url"),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn params(auth: &str, token: &str) -> Params {
        let Value::Object(m) = json!({"clientId": "cid", "clientSecret": "sec", "authUrl": auth, "tokenUrl": token,
            "scope": "a b", "authParams": "access_type=offline&prompt=consent", "tenant": "t1"})
        else {
            unreachable!()
        };
        Params { data: m }
    }

    #[tokio::test]
    async fn full_flow_against_a_fake_provider() {
        // fake token endpoint
        let tl = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let token_url = format!(
            "http://{}/token/{{{{$credentials.tenant}}}}",
            tl.local_addr().unwrap()
        );
        let seen = std::sync::Arc::new(Mutex::new(String::new()));
        let seen2 = seen.clone();
        tokio::spawn(async move {
            let (mut s, _) = tl.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = s.read(&mut buf).await.unwrap();
            *seen2.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = r#"{"access_token":"AT","refresh_token":"RT","instance_url":"https://org.example"}"#;
            let _ = s
                .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes())
                .await;
        });
        let b = begin(params("https://auth.example/authorize", &token_url))
            .await
            .unwrap();
        assert!(
            b.auth_url
                .starts_with("https://auth.example/authorize?response_type=code&client_id=cid")
        );
        assert!(b.auth_url.contains("code_challenge_method=S256"));
        assert!(b.auth_url.contains("access_type=offline"));
        assert!(b.auth_url.contains("scope=a%20b"));

        // the "browser": call the redirect with a wrong state (ignored), then the right one
        let port = b
            .redirect_uri
            .rsplit(':')
            .next()
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .to_owned();
        let state = b.state.clone();
        let hit = move |q: String| {
            let port = port.clone();
            async move {
                let mut c = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
                    .await
                    .unwrap();
                c.write_all(format!("GET /callback?{q} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
                let mut out = Vec::new();
                let _ = c.read_to_end(&mut out).await;
                String::from_utf8_lossy(&out).into_owned()
            }
        };
        let r = hit("code=bad&state=wrong".into()).await;
        assert!(r.contains("Sign-in failed"));
        let r = hit(format!("code=the%20code&state={state}")).await;
        assert!(r.contains("Signed in"));

        let t = finish(&b.state).await.unwrap();
        assert_eq!(t.refresh_token.as_deref(), Some("RT"));
        assert_eq!(t.access_token.as_deref(), Some("AT"));
        assert_eq!(t.instance_url.as_deref(), Some("https://org.example"));
        let req = seen.lock().unwrap().clone();
        assert!(req.starts_with("POST /token/t1 "), "{req}");
        assert!(req.contains("grant_type=authorization_code"));
        assert!(req.contains("code=the+code") || req.contains("code=the%20code"));
        assert!(req.contains("code_verifier="));
        assert!(finish(&b.state).await.is_err(), "a flow is single use");
    }

    #[tokio::test]
    async fn cancel_ends_the_wait() {
        let b = begin(params("https://auth.example/a", "http://127.0.0.1:9/t"))
            .await
            .unwrap();
        cancel(&b.state);
        assert!(finish(&b.state).await.unwrap_err().contains("cancel"));
    }
}
