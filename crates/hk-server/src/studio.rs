//! `/api/v1/studio/*`: the Video Studio of this instance for remote apps.
//!
//! * `POST /studio/rpc/{method}`: JSON in, JSON out; the same methods the in-process app calls.
//! * `GET  /studio/events/{id}`: server-sent events of one project.
//! * `PUT  /studio/upload?name=`: stream a video in; returns the server-side path to import.
//! * `POST /studio/ticket`: a short-lived ticket so a media player can fetch files without the
//!   API token (players cannot send auth headers reliably).
//! * `GET  /studio/media?path=&ticket=`: serve a file the Studio owns, with Range support.
//!
//! The Studio itself is behind [`StudioApi`]; hk-server knows nothing about video.

use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

use crate::api::AppState;
use crate::auth::{Principal, Scope, authorize, token_from};
use crate::error::ApiError;

/// Largest accepted upload.
const MAX_UPLOAD: u64 = 16 * 1024 * 1024 * 1024;
const TICKET_TTL: Duration = Duration::from_secs(3600);

#[async_trait]
pub trait StudioApi: Send + Sync {
    /// Run a Studio method; `Invalid`/`NotFound` style failures map to 400/404.
    async fn call(&self, method: &str, params: Value) -> Result<Value, ApiError>;
    async fn events(&self, id: &str) -> Result<BoxStream<'static, String>, ApiError>;
    /// Directories media requests may read from.
    fn media_roots(&self) -> Vec<PathBuf>;
    /// Where uploads are stored.
    fn upload_dir(&self) -> PathBuf;
}

/// Tickets are random, in memory only (a restart invalidates them; clients just ask again).
#[derive(Default)]
pub struct Tickets(Mutex<HashMap<String, Instant>>);

impl std::fmt::Debug for Tickets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tickets")
    }
}

impl Tickets {
    fn issue(&self) -> String {
        let mut b = [0u8; 24];
        let _ = getrandom::fill(&mut b);
        let t: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let mut m = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        m.retain(|_, exp| *exp > Instant::now());
        m.insert(t.clone(), Instant::now() + TICKET_TTL);
        t
    }

    fn valid(&self, t: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(t)
            .is_some_and(|exp| *exp > Instant::now())
    }
}

fn studio(s: &AppState) -> Result<&Arc<dyn StudioApi>, ApiError> {
    s.studio
        .as_ref()
        .ok_or_else(|| ApiError::NotFound("this instance has no Video Studio".into()))
}

/// Read-only methods need `read`; settings and keys need `admin`; the rest `run`.
fn scope_for(method: &str) -> Scope {
    match method {
        "projects" | "project" | "preview" | "history" | "templates" | "check_tools"
        | "render_path" | "thumbnail" | "whisper_status" | "deps_status" => Scope::Read,
        "get_settings" => Scope::Run,
        "save_settings" | "set_key" | "whisper_pull" | "whisper_cancel" | "whisper_remove"
        | "deps_install" | "deps_cancel" => Scope::Admin,
        _ => Scope::Run,
    }
}

pub(crate) async fn rpc(
    p: Principal,
    State(s): State<AppState>,
    Path(method): Path<String>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, ApiError> {
    p.require(scope_for(&method))?;
    let params = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    Ok(Json(
        json!({ "result": studio(&s)?.call(&method, params).await? }),
    ))
}

pub(crate) async fn events(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>>, ApiError> {
    p.require(Scope::Read)?;
    let stream = studio(&s)?.events(&id).await?;
    Ok(Sse::new(stream.map(|m| Ok(Event::default().data(m)))).keep_alive(KeepAlive::default()))
}

#[derive(Deserialize)]
pub(crate) struct UploadQuery {
    name: Option<String>,
}

fn safe_name(n: &str) -> String {
    let base = n.rsplit(['/', '\\']).next().unwrap_or("video");
    let s: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let s = s.trim_start_matches('.').to_owned();
    if s.is_empty() { "video".into() } else { s }
}

pub(crate) async fn upload(
    p: Principal,
    State(s): State<AppState>,
    Query(q): Query<UploadQuery>,
    body: Body,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Run)?;
    let dir = studio(&s)?.upload_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ApiError::Internal(format!("upload dir: {e}")))?;
    let mut id = [0u8; 6];
    let _ = getrandom::fill(&mut id);
    let tag: String = id.iter().map(|x| format!("{x:02x}")).collect();
    let path = dir.join(format!(
        "{tag}-{}",
        safe_name(q.name.as_deref().unwrap_or("video"))
    ));
    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|e| ApiError::Internal(format!("upload: {e}")))?;
    let mut stream = body.into_data_stream();
    let mut total: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::BadRequest(format!("upload interrupted: {e}")))?;
        total += chunk.len() as u64;
        if total > MAX_UPLOAD {
            drop(file);
            let _ = tokio::fs::remove_file(&path).await;
            return Err(ApiError::BadRequest("upload is too large".into()));
        }
        if let Err(e) = file.write_all(&chunk).await {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(ApiError::Internal(format!("upload: {e}")));
        }
    }
    file.flush()
        .await
        .map_err(|e| ApiError::Internal(format!("upload: {e}")))?;
    Ok(Json(
        json!({ "path": path.to_string_lossy(), "bytes": total }),
    ))
}

pub(crate) async fn ticket(
    p: Principal,
    State(s): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Read)?;
    studio(&s)?;
    Ok(Json(
        json!({ "ticket": s.tickets.issue(), "ttl_secs": TICKET_TTL.as_secs() }),
    ))
}

#[derive(Deserialize)]
pub(crate) struct MediaQuery {
    path: String,
    ticket: Option<String>,
    download: Option<bool>,
}

fn mime_of(p: &FsPath) -> &'static str {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("m4a" | "aac") => "audio/mp4",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("srt" | "vtt" | "txt") => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// `bytes=a-b` / `bytes=a-` / `bytes=-n` against a file of `len` bytes.
fn parse_range(h: &str, len: u64) -> Option<(u64, u64)> {
    let spec = h.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (len.saturating_sub(n), len.checked_sub(1)?)
        }
        (a, "") => (a.parse().ok()?, len.checked_sub(1)?),
        (a, b) => (
            a.parse().ok()?,
            b.parse::<u64>().ok()?.min(len.checked_sub(1)?),
        ),
    };
    (start <= end && end < len).then_some((start, end))
}

pub(crate) async fn media(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<MediaQuery>,
) -> Result<Response, ApiError> {
    // A ticket (players) or a normal bearer token.
    let ticket_ok = q.ticket.as_deref().is_some_and(|t| s.tickets.valid(t));
    if !ticket_ok {
        authorize(&s, token_from(&headers))
            .map_err(|_| ApiError::Unauthorized)?
            .require(Scope::Read)?;
    }
    let st = studio(&s)?;
    let want = FsPath::new(&q.path);
    let real = tokio::fs::canonicalize(want)
        .await
        .map_err(|_| ApiError::NotFound("no such file".into()))?;
    let mut allowed = false;
    for root in st.media_roots() {
        if let Ok(r) = tokio::fs::canonicalize(&root).await
            && real.starts_with(&r)
        {
            allowed = true;
            break;
        }
    }
    if !allowed {
        return Err(ApiError::NotFound("no such file".into()));
    }
    let meta = tokio::fs::metadata(&real)
        .await
        .map_err(|_| ApiError::NotFound("no such file".into()))?;
    if !meta.is_file() {
        return Err(ApiError::NotFound("no such file".into()));
    }
    let len = meta.len();
    let mut file = tokio::fs::File::open(&real)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|h| parse_range(h, len));
    let (status, start, end) = match range {
        Some(Some((a, b))) => (StatusCode::PARTIAL_CONTENT, a, b),
        Some(None) => {
            let mut r = Response::new(Body::empty());
            *r.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            r.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{len}"))
                    .unwrap_or(HeaderValue::from_static("bytes */0")),
            );
            return Ok(r);
        }
        None => (StatusCode::OK, 0, len.saturating_sub(1)),
    };
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    let count = if len == 0 { 0 } else { end - start + 1 };
    let body = Body::from_stream(ReaderStream::new(file.take(count)));
    let mut res = Response::new(body);
    *res.status_mut() = status;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(mime_of(&real)),
    );
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(count));
    if status == StatusCode::PARTIAL_CONTENT {
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{len}")) {
            h.insert(header::CONTENT_RANGE, v);
        }
    }
    if q.download == Some(true) {
        let name = real
            .file_name()
            .and_then(|n| n.to_str())
            .map(safe_name)
            .unwrap_or_else(|| "file".into());
        if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
            h.insert(header::CONTENT_DISPOSITION, v);
        }
    }
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=60"),
    );
    Ok(res.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some((0, 9)));
        assert_eq!(parse_range("bytes=90-", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=50-500", 100), Some((50, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
    }

    #[test]
    fn names_are_flattened() {
        assert_eq!(safe_name("../../etc/pass wd.mp4"), "pass_wd.mp4");
        assert_eq!(safe_name(".."), "video");
        assert_eq!(safe_name("a\\b\\clip.MOV"), "clip.MOV");
    }
}
