//! Official-API publishers beyond YouTube (P9.39): TikTok Content Posting, Instagram Graph
//! (Reels, resumable upload), X API v2 (chunked media upload + post), LinkedIn Posts API.
//!
//! No OAuth flows here: the user pastes a user access token minted in each platform's developer
//! console (plus the account/author id where the API needs one). Tokens live in the secret store.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{Result, StudioError};
use crate::publish::{Privacy, Published, Publisher};
use crate::social::PlatformPost;

/// Per-platform credentials. `account` = Instagram business user id / LinkedIn author URN.
#[derive(Clone)]
pub struct PlatformCreds {
    pub token: String,
    pub account: Option<String>,
}

impl std::fmt::Debug for PlatformCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformCreds")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

pub const PLATFORMS: &[&str] = &["tiktok", "instagram_reels", "x", "linkedin"];

fn http(what: &str) -> impl Fn(reqwest::Error) -> StudioError + '_ {
    move |e| StudioError::Http(format!("{what}: {e}"))
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(1800))
        .build()
        .unwrap_or_default()
}

async fn json_ok(what: &str, r: reqwest::Response) -> Result<Value> {
    let s = r.status();
    let body = r.text().await.unwrap_or_default();
    if !s.is_success() {
        return Err(StudioError::Http(format!(
            "{what} ({s}): {}",
            body.chars().take(400).collect::<String>()
        )));
    }
    Ok(serde_json::from_str(&body).unwrap_or(Value::Null))
}

/// Caption text: description + hashtags, trimmed to the platform limit.
pub fn caption(post: &PlatformPost, limit: usize) -> String {
    let mut text = if post.description.trim().is_empty() {
        post.title.trim().to_string()
    } else {
        post.description.trim().to_string()
    };
    let tags = post.hashtags.join(" ");
    if !tags.is_empty() && text.chars().count() + 1 + tags.chars().count() <= limit {
        text.push(if text.is_empty() { ' ' } else { '\n' });
        text.push_str(&tags);
    }
    text.trim().chars().take(limit).collect()
}

/// Build the publisher for `platform`.
pub fn publisher(platform: &str, creds: PlatformCreds) -> Result<Box<dyn Publisher>> {
    let need_account = |p: &str| {
        creds
            .account
            .clone()
            .filter(|a| !a.trim().is_empty())
            .ok_or_else(|| StudioError::Invalid(format!("{p}: account id is not set")))
    };
    Ok(match platform {
        "tiktok" => Box::new(Tiktok {
            token: creds.token.clone(),
            base: "https://open.tiktokapis.com".into(),
        }),
        "instagram_reels" => Box::new(Instagram {
            token: creds.token.clone(),
            user: need_account("instagram")?,
            graph: "https://graph.facebook.com/v21.0".into(),
            rupload: "https://rupload.facebook.com/ig-api-upload/v21.0".into(),
        }),
        "x" => Box::new(X {
            token: creds.token.clone(),
            base: "https://api.x.com".into(),
        }),
        "linkedin" => Box::new(Linkedin {
            token: creds.token.clone(),
            author: need_account("linkedin")?,
            base: "https://api.linkedin.com".into(),
        }),
        o => return Err(StudioError::Invalid(format!("unknown platform `{o}`"))),
    })
}

struct Tiktok {
    token: String,
    base: String,
}

#[async_trait]
impl Publisher for Tiktok {
    fn platform(&self) -> &'static str {
        "tiktok"
    }

    async fn publish(
        &self,
        video: &Path,
        post: &PlatformPost,
        privacy: Privacy,
    ) -> Result<Published> {
        let bytes = tokio::fs::read(video).await?;
        let len = bytes.len() as u64;
        let c = client();
        // Unaudited apps may only post SELF_ONLY; public needs app review.
        let level = match privacy {
            Privacy::Public => "PUBLIC_TO_EVERYONE",
            _ => "SELF_ONLY",
        };
        let init = c
            .post(format!("{}/v2/post/publish/video/init/", self.base))
            .bearer_auth(&self.token)
            .json(&json!({
                "post_info": {"title": caption(post, 2200), "privacy_level": level},
                "source_info": {"source": "FILE_UPLOAD", "video_size": len, "chunk_size": len, "total_chunk_count": 1},
            }))
            .send()
            .await
            .map_err(http("tiktok init"))?;
        let v = json_ok("tiktok init", init).await?;
        let url = v["data"]["upload_url"]
            .as_str()
            .ok_or_else(|| StudioError::Http("tiktok init: no upload_url".into()))?;
        let id = v["data"]["publish_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let put = c
            .put(url)
            .header("Content-Type", "video/mp4")
            .header(
                "Content-Range",
                format!("bytes 0-{}/{len}", len.saturating_sub(1)),
            )
            .body(bytes)
            .send()
            .await
            .map_err(http("tiktok upload"))?;
        if !put.status().is_success() {
            return Err(StudioError::Http(format!(
                "tiktok upload ({})",
                put.status()
            )));
        }
        Ok(Published {
            id,
            url: "https://www.tiktok.com/".into(),
        })
    }
}

struct Instagram {
    token: String,
    user: String,
    graph: String,
    rupload: String,
}

#[async_trait]
impl Publisher for Instagram {
    fn platform(&self) -> &'static str {
        "instagram_reels"
    }

    async fn publish(
        &self,
        video: &Path,
        post: &PlatformPost,
        _privacy: Privacy,
    ) -> Result<Published> {
        let bytes = tokio::fs::read(video).await?;
        let c = client();
        let create = c
            .post(format!("{}/{}/media", self.graph, self.user))
            .query(&[
                ("media_type", "REELS"),
                ("upload_type", "resumable"),
                ("caption", &caption(post, 2200)),
                ("access_token", &self.token),
            ])
            .send()
            .await
            .map_err(http("instagram container"))?;
        let v = json_ok("instagram container", create).await?;
        let container = v["id"]
            .as_str()
            .ok_or_else(|| StudioError::Http("instagram: no container id".into()))?
            .to_string();
        let up = c
            .post(format!("{}/{container}", self.rupload))
            .header("Authorization", format!("OAuth {}", self.token))
            .header("offset", "0")
            .header("file_size", bytes.len().to_string())
            .body(bytes)
            .send()
            .await
            .map_err(http("instagram upload"))?;
        json_ok("instagram upload", up).await?;
        // Processing is async; poll until FINISHED (≈ 10 min cap).
        for _ in 0..120 {
            let s = c
                .get(format!("{}/{container}", self.graph))
                .query(&[("fields", "status_code"), ("access_token", &self.token)])
                .send()
                .await
                .map_err(http("instagram status"))?;
            match json_ok("instagram status", s).await?["status_code"].as_str() {
                Some("FINISHED") => break,
                Some("ERROR") | Some("EXPIRED") => {
                    return Err(StudioError::Http("instagram: processing failed".into()));
                }
                _ => tokio::time::sleep(Duration::from_secs(5)).await,
            }
        }
        let publish = c
            .post(format!("{}/{}/media_publish", self.graph, self.user))
            .query(&[
                ("creation_id", container.as_str()),
                ("access_token", &self.token),
            ])
            .send()
            .await
            .map_err(http("instagram publish"))?;
        let id = json_ok("instagram publish", publish).await?["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        Ok(Published {
            url: format!("https://www.instagram.com/reel/{id}/"),
            id,
        })
    }
}

struct X {
    token: String,
    base: String,
}

const X_CHUNK: usize = 4 * 1024 * 1024;

#[async_trait]
impl Publisher for X {
    fn platform(&self) -> &'static str {
        "x"
    }

    async fn publish(
        &self,
        video: &Path,
        post: &PlatformPost,
        _privacy: Privacy,
    ) -> Result<Published> {
        let bytes = tokio::fs::read(video).await?;
        let c = client();
        let init = c
            .post(format!("{}/2/media/upload/initialize", self.base))
            .bearer_auth(&self.token)
            .json(&json!({"media_type": "video/mp4", "total_bytes": bytes.len(), "media_category": "tweet_video"}))
            .send()
            .await
            .map_err(http("x init"))?;
        let media = json_ok("x init", init).await?["data"]["id"]
            .as_str()
            .ok_or_else(|| StudioError::Http("x init: no media id".into()))?
            .to_string();
        for (i, chunk) in bytes.chunks(X_CHUNK).enumerate() {
            let form = reqwest::multipart::Form::new()
                .text("segment_index", i.to_string())
                .part(
                    "media",
                    reqwest::multipart::Part::bytes(chunk.to_vec()).file_name("clip.mp4"),
                );
            let r = c
                .post(format!("{}/2/media/upload/{media}/append", self.base))
                .bearer_auth(&self.token)
                .multipart(form)
                .send()
                .await
                .map_err(http("x append"))?;
            if !r.status().is_success() {
                return Err(StudioError::Http(format!("x append ({})", r.status())));
            }
        }
        let fin = c
            .post(format!("{}/2/media/upload/{media}/finalize", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(http("x finalize"))?;
        let mut state = json_ok("x finalize", fin).await?["data"]["processing_info"].clone();
        for _ in 0..120 {
            match state["state"].as_str() {
                None | Some("succeeded") => break,
                Some("failed") => {
                    return Err(StudioError::Http("x: media processing failed".into()));
                }
                _ => {}
            }
            let wait = state["check_after_secs"].as_u64().unwrap_or(5).clamp(1, 30);
            tokio::time::sleep(Duration::from_secs(wait)).await;
            let s = c
                .get(format!("{}/2/media/upload", self.base))
                .query(&[("command", "STATUS"), ("media_id", media.as_str())])
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(http("x status"))?;
            state = json_ok("x status", s).await?["data"]["processing_info"].clone();
        }
        let tweet = c
            .post(format!("{}/2/tweets", self.base))
            .bearer_auth(&self.token)
            .json(&json!({"text": caption(post, 280), "media": {"media_ids": [media]}}))
            .send()
            .await
            .map_err(http("x post"))?;
        let id = json_ok("x post", tweet).await?["data"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        Ok(Published {
            url: format!("https://x.com/i/status/{id}"),
            id,
        })
    }
}

struct Linkedin {
    token: String,
    author: String,
    base: String,
}

#[async_trait]
impl Publisher for Linkedin {
    fn platform(&self) -> &'static str {
        "linkedin"
    }

    async fn publish(
        &self,
        video: &Path,
        post: &PlatformPost,
        privacy: Privacy,
    ) -> Result<Published> {
        let bytes = tokio::fs::read(video).await?;
        let c = client();
        let req = |b: reqwest::RequestBuilder| {
            b.bearer_auth(&self.token)
                .header("LinkedIn-Version", "202410")
                .header("X-Restli-Protocol-Version", "2.0.0")
        };
        let init = req(c.post(format!("{}/rest/videos?action=initializeUpload", self.base)))
            .json(&json!({"initializeUploadRequest": {"owner": self.author, "fileSizeBytes": bytes.len(), "uploadCaptions": false, "uploadThumbnail": false}}))
            .send()
            .await
            .map_err(http("linkedin init"))?;
        let v = json_ok("linkedin init", init).await?;
        let video_urn = v["value"]["video"]
            .as_str()
            .ok_or_else(|| StudioError::Http("linkedin init: no video urn".into()))?
            .to_string();
        let token = v["value"]["uploadToken"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let mut etags = Vec::new();
        for ins in v["value"]["uploadInstructions"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let (Some(url), Some(a), Some(b)) = (
                ins["uploadUrl"].as_str(),
                ins["firstByte"].as_u64(),
                ins["lastByte"].as_u64(),
            ) else {
                continue;
            };
            let part = bytes
                .get(a as usize..=(b as usize).min(bytes.len().saturating_sub(1)))
                .unwrap_or_default()
                .to_vec();
            let r = c
                .put(url)
                .header("Content-Type", "application/octet-stream")
                .body(part)
                .send()
                .await
                .map_err(http("linkedin upload"))?;
            if !r.status().is_success() {
                return Err(StudioError::Http(format!(
                    "linkedin upload ({})",
                    r.status()
                )));
            }
            etags.push(
                r.headers()
                    .get("etag")
                    .and_then(|e| e.to_str().ok())
                    .unwrap_or_default()
                    .to_string(),
            );
        }
        let fin = req(c.post(format!("{}/rest/videos?action=finalizeUpload", self.base)))
            .json(&json!({"finalizeUploadRequest": {"video": video_urn, "uploadToken": token, "uploadedPartIds": etags}}))
            .send()
            .await
            .map_err(http("linkedin finalize"))?;
        json_ok("linkedin finalize", fin).await?;
        let visibility = match privacy {
            Privacy::Public => "PUBLIC",
            _ => "CONNECTIONS",
        };
        let r = req(c.post(format!("{}/rest/posts", self.base)))
            .json(&json!({
                "author": self.author,
                "commentary": caption(post, 3000),
                "visibility": visibility,
                "distribution": {"feedDistribution": "MAIN_FEED", "targetEntities": [], "thirdPartyDistributionChannels": []},
                "content": {"media": {"title": post.title, "id": video_urn}},
                "lifecycleState": "PUBLISHED",
                "isReshareDisabledByAuthor": false,
            }))
            .send()
            .await
            .map_err(http("linkedin post"))?;
        if !r.status().is_success() {
            let s = r.status();
            let body = r.text().await.unwrap_or_default();
            return Err(StudioError::Http(format!("linkedin post ({s}): {body}")));
        }
        let id = r
            .headers()
            .get("x-restli-id")
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default()
            .to_string();
        Ok(Published {
            url: format!("https://www.linkedin.com/feed/update/{id}/"),
            id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_respects_limit() {
        let p = PlatformPost {
            title: "T".into(),
            description: "a".repeat(275),
            hashtags: vec!["#one".into(), "#two".into()],
        };
        let c = caption(&p, 280);
        assert_eq!(c.chars().count(), 275, "tags dropped when they don't fit");
        let short = PlatformPost {
            description: "hi".into(),
            ..p
        };
        assert_eq!(caption(&short, 280), "hi\n#one #two");
    }
}
