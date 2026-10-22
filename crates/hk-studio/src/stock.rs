use std::path::Path;

use futures::StreamExt;
use hk_store::{Secret, SecretStore};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use crate::{Result, Social, invalid};

pub(crate) async fn search(
    http: &reqwest::Client,
    secrets: &dyn SecretStore,
    provider: &str,
    query: &str,
) -> Result<Value> {
    let key = secrets
        .get(&format!("studio/{provider}"))?
        .ok_or_else(|| invalid(format!("Add your {provider} API key in Studio connections")))?;
    let result: Value = match provider {
        "pexels" => {
            http.get("https://api.pexels.com/videos/search")
                .header("Authorization", key.expose())
                .query(&[("query", query), ("per_page", "12")])
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?
        }
        "pixabay" => {
            http.get("https://pixabay.com/api/videos/")
                .query(&[("key", key.expose()), ("q", query), ("per_page", "12")])
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?
        }
        _ => return Err(invalid("Choose pexels or pixabay")),
    };
    let list = if provider == "pexels" {
        result["videos"].as_array()
    } else {
        result["hits"].as_array()
    };
    let assets: Vec<Value> = list
        .into_iter()
        .flatten()
        .filter_map(|video| {
            let (url, attribution) = if provider == "pexels" {
                let file = video["video_files"]
                    .as_array()?
                    .iter()
                    .filter(|f| f["file_type"] == "video/mp4")
                    .min_by_key(|f| (f["width"].as_u64().unwrap_or(0) as i64 - 1080).abs())?;
                (file["link"].as_str()?, video["url"].as_str().unwrap_or(""))
            } else {
                (
                    video["videos"]["medium"]["url"].as_str()?,
                    video["pageURL"].as_str().unwrap_or(""),
                )
            };
            Some(json!({"url":url,"attribution":attribution,"provider":provider,"id":video["id"]}))
        })
        .collect();
    Ok(json!({"assets":assets}))
}

pub(crate) async fn download(http: &reqwest::Client, url: &str, path: &Path) -> Result<()> {
    let url = reqwest::Url::parse(url).map_err(|_| invalid("invalid stock asset URL"))?;
    let host = url.host_str().unwrap_or("");
    if url.scheme() != "https"
        || !(host == "videos.pexels.com"
            || host.ends_with(".pexels.com")
            || host == "pixabay.com"
            || host.ends_with(".pixabay.com"))
    {
        return Err(invalid(
            "Select a stock URL returned by Pexels or Pixabay; use a local path for other media",
        ));
    }
    let mut response = http
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes_stream();
    let temp = path.with_extension("download");
    let result = async {
        let mut file = tokio::fs::File::create(&temp).await?;
        let mut size = 0;
        while let Some(chunk) = response.next().await {
            let chunk = chunk?;
            size += chunk.len();
            if size > 512 * 1024 * 1024 {
                return Err(invalid("Stock asset exceeds 512 MiB"));
            }
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        tokio::fs::rename(&temp, path).await?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(temp).await;
    }
    result
}

/// Resumable YouTube upload. The caller must obtain explicit user confirmation first.
pub(crate) async fn youtube(
    http: &reqwest::Client,
    token: &Secret,
    path: &Path,
    metadata: &Social,
    privacy: &str,
) -> Result<Value> {
    if !["private", "unlisted", "public"].contains(&privacy) {
        return Err(invalid("invalid YouTube privacy"));
    }
    let file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    let response=http.post("https://www.googleapis.com/upload/youtube/v3/videos")
        .query(&[("uploadType","resumable"),("part","snippet,status")])
        .bearer_auth(token.expose()).header("X-Upload-Content-Type","video/mp4")
        .header("X-Upload-Content-Length",size)
        .json(&json!({"snippet":{"title":metadata.title,"description":format!("{}\n\n{}",metadata.description,metadata.hashtags.join(" "))},
            "status":{"privacyStatus":privacy,"selfDeclaredMadeForKids":false}}))
        .send().await?.error_for_status()?;
    let location = response
        .headers()
        .get("location")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| invalid("YouTube omitted upload URL"))?;
    let url = reqwest::Url::parse(location).map_err(|_| invalid("invalid YouTube upload URL"))?;
    if url.scheme() != "https"
        || !url
            .host_str()
            .is_some_and(|h| h == "www.googleapis.com" || h == "youtube.googleapis.com")
    {
        return Err(invalid("YouTube returned an unexpected upload host"));
    }
    let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
    Ok(http
        .put(url)
        .bearer_auth(token.expose())
        .header("Content-Type", "video/mp4")
        .header("Content-Length", size)
        .body(body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}
