//! Publishing. YouTube first (Data API v3, resumable upload with a user-supplied OAuth
//! refresh token); other platforms need app approval, so for them Studio writes an
//! export package (`StudioService::export_package`) instead.
//!
//! Uploads default to `private`: the user reviews the video on the platform before it goes public.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{Result, StudioError};
use crate::social::PlatformPost;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Privacy {
    Private,
    Unlisted,
    Public,
}

impl Privacy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Unlisted => "unlisted",
            Self::Public => "public",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Published {
    pub id: String,
    pub url: String,
}

#[async_trait]
pub trait Publisher: Send + Sync {
    fn platform(&self) -> &'static str;
    async fn publish(
        &self,
        video: &Path,
        post: &PlatformPost,
        privacy: Privacy,
    ) -> Result<Published>;
}

/// Google OAuth client + the user's refresh token (create them in Google Cloud Console).
#[derive(Clone)]
pub struct YoutubeCreds {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
}

impl std::fmt::Debug for YoutubeCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YoutubeCreds")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct Youtube {
    creds: YoutubeCreds,
    client: reqwest::Client,
    token_url: String,
    upload_base: String,
    cached: Mutex<Option<(String, Instant)>>,
}

const MAX_UPLOAD: u64 = 2 * 1024 * 1024 * 1024;

impl Youtube {
    pub fn new(creds: YoutubeCreds) -> Self {
        Self {
            creds,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(1800))
                .build()
                .unwrap_or_default(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            upload_base: "https://www.googleapis.com".into(),
            cached: Mutex::new(None),
        }
    }

    /// Point at other endpoints (tests).
    pub fn with_endpoints(
        mut self,
        token_url: impl Into<String>,
        upload_base: impl Into<String>,
    ) -> Self {
        self.token_url = token_url.into();
        self.upload_base = upload_base.into().trim_end_matches('/').to_string();
        self
    }

    async fn access_token(&self) -> Result<String> {
        if let Some((t, exp)) = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            if exp > Instant::now() {
                return Ok(t);
            }
        }
        let resp = self
            .client
            .post(&self.token_url)
            .form(&[
                ("client_id", self.creds.client_id.as_str()),
                ("client_secret", self.creds.client_secret.as_str()),
                ("refresh_token", self.creds.refresh_token.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await
            .map_err(|e| StudioError::Http(format!("youtube auth: {e}")))?;
        let status = resp.status();
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| StudioError::Http(format!("youtube auth: {e}")))?;
        let token = v["access_token"].as_str().ok_or_else(|| {
            StudioError::Http(format!(
                "youtube auth failed ({status}): {}",
                v["error_description"]
                    .as_str()
                    .or(v["error"].as_str())
                    .unwrap_or("no access_token")
            ))
        })?;
        let ttl = v["expires_in"].as_u64().unwrap_or(3000).saturating_sub(60);
        *self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((token.to_string(), Instant::now() + Duration::from_secs(ttl)));
        Ok(token.to_string())
    }
}

/// Snippet fields: Shorts get `#Shorts`, tags come from the hashtags.
pub fn youtube_snippet(post: &PlatformPost) -> serde_json::Value {
    let mut title = post.title.trim().to_string();
    if title.is_empty() {
        title = "Untitled".into();
    }
    let mut description = post.description.trim().to_string();
    let tags: Vec<String> = post
        .hashtags
        .iter()
        .map(|t| t.trim_start_matches('#').to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if !description.to_lowercase().contains("#shorts") && !title.to_lowercase().contains("#shorts")
    {
        if !description.is_empty() {
            description.push_str("\n\n");
        }
        description.push_str("#Shorts");
    }
    if !post.hashtags.is_empty() {
        description.push('\n');
        description.push_str(&post.hashtags.join(" "));
    }
    serde_json::json!({
        "title": title.chars().take(100).collect::<String>(),
        "description": description.chars().take(5000).collect::<String>(),
        "tags": tags,
        "categoryId": "22",
    })
}

#[async_trait]
impl Publisher for Youtube {
    fn platform(&self) -> &'static str {
        "youtube_shorts"
    }

    async fn publish(
        &self,
        video: &Path,
        post: &PlatformPost,
        privacy: Privacy,
    ) -> Result<Published> {
        let len = tokio::fs::metadata(video).await?.len();
        if len == 0 || len > MAX_UPLOAD {
            return Err(StudioError::Invalid(format!(
                "video size {len} bytes is not uploadable"
            )));
        }
        let token = self.access_token().await?;
        let meta = serde_json::json!({
            "snippet": youtube_snippet(post),
            "status": {"privacyStatus": privacy.as_str(), "selfDeclaredMadeForKids": false},
        });
        let init = self
            .client
            .post(format!(
                "{}/upload/youtube/v3/videos?uploadType=resumable&part=snippet,status",
                self.upload_base
            ))
            .bearer_auth(&token)
            .header("X-Upload-Content-Type", "video/mp4")
            .header("X-Upload-Content-Length", len.to_string())
            .json(&meta)
            .send()
            .await
            .map_err(|e| StudioError::Http(format!("youtube upload init: {e}")))?;
        if !init.status().is_success() {
            let s = init.status();
            let body = init.text().await.unwrap_or_default();
            return Err(StudioError::Http(format!(
                "youtube upload init {s}: {}",
                body.chars().take(300).collect::<String>()
            )));
        }
        let location = init
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| StudioError::Http("youtube did not return an upload URL".into()))?
            .to_string();
        let bytes = tokio::fs::read(video).await?;
        let up = self
            .client
            .put(&location)
            .bearer_auth(&token)
            .header(reqwest::header::CONTENT_TYPE, "video/mp4")
            .body(bytes)
            .send()
            .await
            .map_err(|e| StudioError::Http(format!("youtube upload: {e}")))?;
        let s = up.status();
        let v: serde_json::Value = up.json().await.unwrap_or_default();
        if !s.is_success() {
            return Err(StudioError::Http(format!(
                "youtube upload {s}: {}",
                v["error"]["message"].as_str().unwrap_or("failed")
            )));
        }
        let id = v["id"]
            .as_str()
            .ok_or_else(|| StudioError::Http("youtube returned no video id".into()))?
            .to_string();
        Ok(Published {
            url: format!("https://www.youtube.com/shorts/{id}"),
            id,
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    fn post() -> PlatformPost {
        PlatformPost {
            title: "Big reveal".into(),
            description: "Watch till the end".into(),
            hashtags: vec!["#podcast".into(), "#fun".into()],
        }
    }

    #[test]
    fn snippet_adds_shorts_and_tags() {
        let s = youtube_snippet(&post());
        assert_eq!(s["title"], "Big reveal");
        assert!(s["description"].as_str().unwrap().contains("#Shorts"));
        assert!(s["description"].as_str().unwrap().contains("#podcast #fun"));
        assert_eq!(s["tags"], serde_json::json!(["podcast", "fun"]));
        let mut p = post();
        p.title = "x #shorts".into();
        p.description = String::new();
        assert!(
            !youtube_snippet(&p)["description"]
                .as_str()
                .unwrap()
                .contains("#Shorts")
        );
        p.title = "t".repeat(300);
        assert_eq!(
            youtube_snippet(&p)["title"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            100
        );
    }

    /// Reads one HTTP request (headers + body); returns (head, body).
    async fn read_req(s: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
        let mut got = Vec::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = s.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
            if let Some(h) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&got[..h]).to_string();
                let len: usize = head
                    .to_lowercase()
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("content-length: ")
                            .and_then(|v| v.trim().parse().ok())
                    })
                    .unwrap_or(0);
                if got.len() >= h + 4 + len {
                    return (head, got[h + 4..h + 4 + len].to_vec());
                }
            }
        }
        (String::new(), Vec::new())
    }

    #[tokio::test]
    async fn uploads_resumably_with_refreshed_token() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let seen = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        let base2 = base.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                let (head, body) = read_req(&mut s).await;
                let first = head.lines().next().unwrap_or("").to_string();
                seen2.lock().unwrap().push(format!(
                    "{first} | {}",
                    String::from_utf8_lossy(&body)
                        .chars()
                        .take(200)
                        .collect::<String>()
                        .replace('\n', " ")
                ));
                let (status, extra, payload) = if first.starts_with("POST /token") {
                    (
                        "200 OK",
                        String::new(),
                        r#"{"access_token":"at-1","expires_in":3600}"#.to_string(),
                    )
                } else if first.starts_with("POST /upload/youtube/v3/videos") {
                    assert!(head.to_lowercase().contains("authorization: bearer at-1"));
                    assert!(
                        String::from_utf8_lossy(&body).contains("\"privacyStatus\":\"private\"")
                    );
                    (
                        "200 OK",
                        format!("location: {base2}/put/123\r\n"),
                        String::new(),
                    )
                } else if first.starts_with("PUT /put/123") {
                    assert_eq!(body, b"FAKEVIDEO");
                    ("200 OK", String::new(), r#"{"id":"vid42"}"#.to_string())
                } else {
                    ("404 Not Found", String::new(), String::new())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
            }
        });
        let dir = std::env::temp_dir().join(format!("hk_pub_{}", crate::project::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("c.mp4");
        std::fs::write(&file, b"FAKEVIDEO").unwrap();
        let yt = Youtube::new(YoutubeCreds {
            client_id: "id".into(),
            client_secret: "sec".into(),
            refresh_token: "rt".into(),
        })
        .with_endpoints(format!("{base}/token"), base.clone());
        let out = yt.publish(&file, &post(), Privacy::Private).await.unwrap();
        assert_eq!(out.id, "vid42");
        assert_eq!(out.url, "https://www.youtube.com/shorts/vid42");
        // Token cached: a second upload does not hit /token again.
        yt.publish(&file, &post(), Privacy::Private).await.unwrap();
        let log = seen.lock().unwrap().clone();
        assert_eq!(
            log.iter().filter(|l| l.starts_with("POST /token")).count(),
            1,
            "{log:?}"
        );
        assert!(log[0].contains("grant_type=refresh_token"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn auth_failure_is_reported() {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let _ = read_req(&mut s).await;
            let p = r#"{"error":"invalid_grant","error_description":"Token has been revoked"}"#;
            let _ = s.write_all(format!("HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{p}", p.len()).as_bytes()).await;
        });
        let dir = std::env::temp_dir().join(format!("hk_pub2_{}", crate::project::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("c.mp4");
        std::fs::write(&file, b"x").unwrap();
        let yt = Youtube::new(YoutubeCreds {
            client_id: "i".into(),
            client_secret: "s".into(),
            refresh_token: "r".into(),
        })
        .with_endpoints(format!("{base}/token"), base);
        let err = yt
            .publish(&file, &post(), Privacy::Private)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("revoked"), "{err}");
        assert!(
            yt.publish(&dir.join("missing.mp4"), &post(), Privacy::Private)
                .await
                .is_err()
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
