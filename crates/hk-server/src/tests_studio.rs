//! `/api/v1/studio`: scopes, uploads, tickets and ranged media against a fake Studio.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::stream::{self, BoxStream, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::tests::Fake;
use crate::*;

const READ: &str = "tok-read";
const RUN: &str = "tok-run";
const ADMIN: &str = "tok-admin";

struct FakeStudio {
    root: PathBuf,
}

#[async_trait]
impl StudioApi for FakeStudio {
    async fn call(&self, method: &str, params: Value) -> Result<Value, ApiError> {
        match method {
            "echo" | "projects" | "save_settings" => Ok(params),
            _ => Err(ApiError::BadRequest(format!(
                "unknown Studio method `{method}`"
            ))),
        }
    }
    async fn events(&self, id: &str) -> Result<BoxStream<'static, String>, ApiError> {
        Ok(stream::iter(vec![format!(r#"{{"kind":"edit","id":"{id}"}}"#)]).boxed())
    }
    fn media_roots(&self) -> Vec<PathBuf> {
        vec![self.root.clone()]
    }
    fn upload_dir(&self) -> PathBuf {
        self.root.join("_uploads")
    }
}

fn app(root: &std::path::Path) -> axum::Router {
    let auth = Auth::new()
        .with_token(Scope::Read, READ)
        .with_token(Scope::Run, RUN)
        .with_token(Scope::Admin, ADMIN);
    router(
        AppState::new(Arc::new(Fake::default()), auth).with_studio(Arc::new(FakeStudio {
            root: root.to_path_buf(),
        })),
    )
}

async fn send(
    app: &axum::Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    (
        status,
        headers,
        res.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

fn post(uri: &str, tok: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {tok}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str, tok: Option<&str>, range: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(Method::GET).uri(uri);
    if let Some(t) = tok {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    if let Some(r) = range {
        b = b.header(header::RANGE, r);
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn rpc_scopes_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let (st, _, body) = send(
        &app,
        post("/api/v1/studio/rpc/projects", READ, json!({"a": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["result"],
        json!({"a": 1})
    );
    // run-scope method with a read token
    let (st, ..) = send(&app, post("/api/v1/studio/rpc/echo", READ, json!({}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // settings are admin only
    let (st, ..) = send(
        &app,
        post("/api/v1/studio/rpc/save_settings", RUN, json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, ..) = send(
        &app,
        post("/api/v1/studio/rpc/save_settings", ADMIN, json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _, body) = send(&app, post("/api/v1/studio/rpc/nope", RUN, json!({}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("unknown Studio method"));
}

#[tokio::test]
async fn upload_ticket_and_ranged_media() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());

    // upload (name is flattened), then fetch it back through a ticket
    let req = Request::builder()
        .method(Method::PUT)
        .uri("/api/v1/studio/upload?name=..%2F..%2Fmy%20clip.mp4")
        .header(header::AUTHORIZATION, format!("Bearer {RUN}"))
        .body(Body::from(b"0123456789abcdef".to_vec()))
        .unwrap();
    let (st, _, body) = send(&app, req).await;
    assert_eq!(st, StatusCode::OK);
    let up: Value = serde_json::from_slice(&body).unwrap();
    let path = up["path"].as_str().unwrap().to_owned();
    assert!(
        path.starts_with(dir.path().join("_uploads").to_str().unwrap()),
        "{path}"
    );
    assert!(path.ends_with("my_clip.mp4"), "{path}");

    let (st, ..) = send(&app, post("/api/v1/studio/ticket", READ, json!({}))).await;
    assert_eq!(st, StatusCode::OK);
    let (_, _, body) = send(&app, post("/api/v1/studio/ticket", READ, json!({}))).await;
    let ticket = serde_json::from_slice::<Value>(&body).unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_owned();
    let enc = |s: &str| form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    let url = format!("/api/v1/studio/media?path={}&ticket={ticket}", enc(&path));

    let (st, h, body) = send(&app, get(&url, None, None)).await;
    assert_eq!(
        (st, body.as_slice()),
        (StatusCode::OK, &b"0123456789abcdef"[..])
    );
    assert_eq!(h[header::CONTENT_TYPE], "video/mp4");
    let (st, h, body) = send(&app, get(&url, None, Some("bytes=4-7"))).await;
    assert_eq!(
        (st, body.as_slice()),
        (StatusCode::PARTIAL_CONTENT, &b"4567"[..])
    );
    assert_eq!(h[header::CONTENT_RANGE], "bytes 4-7/16");
    let (st, ..) = send(&app, get(&url, None, Some("bytes=99-"))).await;
    assert_eq!(st, StatusCode::RANGE_NOT_SATISFIABLE);

    // no ticket and no token, or a bad ticket
    let (st, ..) = send(
        &app,
        get(
            &format!("/api/v1/studio/media?path={}", enc(&path)),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, ..) = send(
        &app,
        get(
            &format!("/api/v1/studio/media?path={}&ticket=bogus", enc(&path)),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // a bearer token works without a ticket
    let (st, ..) = send(
        &app,
        get(
            &format!("/api/v1/studio/media?path={}", enc(&path)),
            Some(READ),
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // outside the Studio's folders: indistinguishable from missing
    let outside = tempfile::NamedTempFile::new().unwrap();
    let (st, ..) = send(
        &app,
        get(
            &format!(
                "/api/v1/studio/media?path={}&ticket={ticket}",
                enc(outside.path().to_str().unwrap())
            ),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let traversal = dir
        .path()
        .join("_uploads")
        .join("..")
        .join("..")
        .join("etc")
        .join("passwd");
    let (st, ..) = send(
        &app,
        get(
            &format!(
                "/api/v1/studio/media?path={}&ticket={ticket}",
                enc(traversal.to_str().unwrap())
            ),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn events_stream_as_sse() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path());
    let (st, h, body) = send(&app, get("/api/v1/studio/events/p1", Some(READ), None)).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        h[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    assert!(String::from_utf8_lossy(&body).contains(r#"data: {"kind":"edit","id":"p1"}"#));
}
