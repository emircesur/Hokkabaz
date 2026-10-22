//! Local HTTP server and credential stub shared by the integration tests.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hk_flow::{CredentialRef, CredentialResolver, FlowError};
use serde_json::{Map, Value};

/// Serves one canned raw response per connection, in order; records raw requests.
pub async fn serve(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        for resp in responses {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).into_owned();
                if let Some(h) = text.find("\r\n\r\n") {
                    let len = text[..h]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if buf.len() >= h + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            seen2
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf).into_owned());
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), seen)
}

pub fn http_resp(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Resolver returning fixed credential data for every reference.
#[derive(Debug)]
pub struct Creds(pub Value);

#[async_trait]
impl CredentialResolver for Creds {
    async fn resolve(
        &self,
        _kind: &str,
        _r: &CredentialRef,
    ) -> Result<Map<String, Value>, FlowError> {
        match &self.0 {
            Value::Object(m) => Ok(m.clone()),
            _ => Ok(Map::new()),
        }
    }
}
