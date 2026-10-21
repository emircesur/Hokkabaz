//! Auth, routing and handler behaviour against a fake runtime, plus one end-to-end pass of
//! `LocalRuntime` over a scripted provider.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hk_llm::{
    CancellationToken, ChatRequest, EventStream, FinishReason, ModelInfo, Provider, StreamEvent,
};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tower::ServiceExt;

use crate::*;

const READ: &str = "tok-read";
const RUN: &str = "tok-run";
const ADMIN: &str = "tok-admin";

#[derive(Default)]
pub(crate) struct Fake {
    sessions: Mutex<Vec<SessionInfo>>,
    calls: Mutex<Vec<String>>,
}

impl Fake {
    fn call(&self, s: impl Into<String>) {
        self.calls.lock().unwrap().push(s.into());
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn find(&self, id: &str) -> Result<SessionInfo, ApiError> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| ApiError::NotFound(format!("session `{id}` not found")))
    }
}

#[async_trait::async_trait]
impl AgentRuntime for Fake {
    async fn list_sessions(&self, limit: u32, offset: u32) -> Result<Vec<SessionInfo>, ApiError> {
        self.call(format!("list {limit} {offset}"));
        Ok(self.sessions.lock().unwrap().clone())
    }
    async fn create_session(&self, req: CreateSession) -> Result<SessionInfo, ApiError> {
        let mut v = self.sessions.lock().unwrap();
        let s = SessionInfo {
            id: format!("s{}", v.len() + 1),
            title: req.title,
            model: req.model,
            cwd: req.cwd,
            created_ms: 1,
            updated_ms: 1,
            event_count: 1,
            turn_count: 0,
            running: false,
        };
        v.push(s.clone());
        Ok(s)
    }
    async fn session(&self, id: &str, after: Option<u64>) -> Result<SessionDetail, ApiError> {
        self.call(format!("get {id} {after:?}"));
        Ok(SessionDetail {
            session: self.find(id)?,
            events: Vec::new(),
            messages: Vec::new(),
            pending_approvals: vec!["appr_0".into()],
        })
    }
    async fn submit(&self, id: &str, req: TurnRequest) -> Result<TurnAccepted, ApiError> {
        self.find(id)?;
        self.call(format!("submit {id} {} {:?} {:?}", req.prompt, req.mode, req.model));
        Ok(TurnAccepted { session_id: id.into(), queued: false })
    }
    async fn inject(&self, id: &str, text: String) -> Result<(), ApiError> {
        self.find(id)?;
        self.call(format!("inject {id} {text}"));
        Ok(())
    }
    async fn cancel(&self, id: &str) -> Result<(), ApiError> {
        self.find(id)?;
        self.call(format!("cancel {id}"));
        Ok(())
    }
    async fn respond_approval(&self, id: &str, d: ApprovalDecision) -> Result<(), ApiError> {
        if id != "appr_0" {
            return Err(ApiError::NotFound(format!("no pending approval `{id}`")));
        }
        self.call(format!("approve {id} {}", d.approved));
        Ok(())
    }
    async fn models(&self) -> Result<Vec<ModelInfo>, ApiError> {
        Ok(vec![ModelInfo {
            id: "m".into(),
            provider: "p".into(),
            name: "M".into(),
            context_window: 10,
            max_output: None,
            capabilities: hk_llm::Capabilities::default(),
            cost: None,
        }])
    }
    fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        broadcast::channel(1).1
    }
}

fn auth() -> Auth {
    Auth::new()
        .with_token(Scope::Read, READ)
        .with_token(Scope::Run, RUN)
        .with_token(Scope::Admin, ADMIN)
}

fn app() -> (axum::Router, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    (router(AppState::new(fake.clone(), auth())), fake)
}

async fn call(
    app: &axum::Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(v) => b.header(header::CONTENT_TYPE, "application/json").body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn health_is_open() {
    let (app, _) = app();
    let (st, body) = call(&app, Method::GET, "/api/v1/health", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn auth_missing_bad_and_malformed_are_401() {
    let (app, _) = app();
    let (st, body) = call(&app, Method::GET, "/api/v1/sessions", None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");

    let (st, _) = call(&app, Method::GET, "/api/v1/sessions", Some("nope"), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Wrong scheme.
    let req = Request::builder()
        .uri("/api/v1/sessions")
        .header(header::AUTHORIZATION, format!("Basic {READ}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(res.headers()[header::WWW_AUTHENTICATE], "Bearer");
}

#[tokio::test]
async fn scopes_gate_mutations() {
    let (app, fake) = app();
    let mutating = [
        (Method::POST, "/api/v1/sessions", json!({})),
        (Method::POST, "/api/v1/sessions/s1/turns", json!({"prompt": "x"})),
        (Method::POST, "/api/v1/sessions/s1/inject", json!({"text": "x"})),
        (Method::POST, "/api/v1/sessions/s1/cancel", json!({})),
        (Method::POST, "/api/v1/approvals/appr_0", json!({"approved": true})),
    ];
    for (m, uri, body) in mutating {
        let (st, b) = call(&app, m, uri, Some(READ), Some(body)).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(b["error"]["code"], "forbidden");
    }
    assert!(fake.calls().is_empty(), "read token must not reach the runtime");

    for t in [READ, RUN, ADMIN] {
        for uri in ["/api/v1/sessions", "/api/v1/models"] {
            assert_eq!(call(&app, Method::GET, uri, Some(t), None).await.0, StatusCode::OK);
        }
    }
    assert_eq!(call(&app, Method::POST, "/api/v1/sessions", Some(ADMIN), None).await.0, StatusCode::CREATED);
}

#[tokio::test]
async fn session_create_list_get() {
    let (app, fake) = app();
    let (st, created) = call(
        &app,
        Method::POST,
        "/api/v1/sessions",
        Some(RUN),
        Some(json!({"title": "t", "model": "m"})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(created["id"], "s1");
    assert_eq!(created["title"], "t");

    // Empty body also creates.
    assert_eq!(call(&app, Method::POST, "/api/v1/sessions", Some(RUN), None).await.0, StatusCode::CREATED);

    let (st, list) = call(&app, Method::GET, "/api/v1/sessions?limit=9999&offset=3", Some(READ), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list["sessions"].as_array().unwrap().len(), 2);
    // Limit is clamped before it reaches the runtime.
    assert!(fake.calls().contains(&"list 200 3".to_owned()));

    let (st, d) = call(&app, Method::GET, "/api/v1/sessions/s1?after_seq=4", Some(READ), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(d["session"]["id"], "s1");
    assert_eq!(d["pending_approvals"][0], "appr_0");
    assert!(fake.calls().contains(&"get s1 Some(4)".to_owned()));

    let (st, e) = call(&app, Method::GET, "/api/v1/sessions/nope", Some(READ), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(e["error"]["code"], "not_found");
}

#[tokio::test]
async fn turn_inject_cancel_approve_routing() {
    let (app, fake) = app();
    call(&app, Method::POST, "/api/v1/sessions", Some(RUN), None).await;

    let (st, b) = call(
        &app,
        Method::POST,
        "/api/v1/sessions/s1/turns",
        Some(RUN),
        Some(json!({"prompt": "hi", "mode": "plan", "model": "m"})),
    )
    .await;
    assert_eq!(st, StatusCode::ACCEPTED);
    assert_eq!(b["session_id"], "s1");
    assert_eq!(b["queued"], false);

    let bad = [
        json!({"prompt": "  "}),
        json!({"prompt": "x", "mode": "yolo"}),
        json!({"prompt": "x", "bogus": 1}),
    ];
    for body in bad {
        let (st, _) = call(&app, Method::POST, "/api/v1/sessions/s1/turns", Some(RUN), Some(body)).await;
        assert!(st == StatusCode::BAD_REQUEST || st == StatusCode::UNPROCESSABLE_ENTITY, "{st}");
    }

    let post = |uri: &'static str, body: Value| {
        let app = app.clone();
        async move { call(&app, Method::POST, uri, Some(RUN), Some(body)).await.0 }
    };
    assert_eq!(post("/api/v1/sessions/s1/inject", json!({"text": "steer"})).await, StatusCode::ACCEPTED);
    assert_eq!(post("/api/v1/sessions/s1/inject", json!({"text": ""})).await, StatusCode::BAD_REQUEST);
    assert_eq!(post("/api/v1/sessions/s1/cancel", json!({})).await, StatusCode::ACCEPTED);
    assert_eq!(post("/api/v1/sessions/zzz/cancel", json!({})).await, StatusCode::NOT_FOUND);
    assert_eq!(post("/api/v1/approvals/appr_0", json!({"approved": false})).await, StatusCode::OK);
    assert_eq!(post("/api/v1/approvals/appr_9", json!({"approved": true})).await, StatusCode::NOT_FOUND);

    let calls = fake.calls();
    for want in [
        "submit s1 hi Some(Plan) Some(\"m\")",
        "inject s1 steer",
        "cancel s1",
        "approve appr_0 false",
    ] {
        assert!(calls.contains(&want.to_owned()), "missing {want}: {calls:?}");
    }
}

#[tokio::test]
async fn unknown_route_and_method() {
    let (app, _) = app();
    assert_eq!(call(&app, Method::GET, "/api/v1/nope", Some(RUN), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, Method::DELETE, "/api/v1/sessions", Some(RUN), None).await.0, StatusCode::METHOD_NOT_ALLOWED);
    // Non-/api/v1 paths (webhooks etc.) aren't ours yet.
    assert_eq!(call(&app, Method::GET, "/webhook/x", None, None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ws_requires_auth_before_upgrade() {
    let (app, _) = app();
    // Plain GET without upgrade headers: auth failure wins over the upgrade error.
    assert_eq!(call(&app, Method::GET, "/api/v1/ws", None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, Method::GET, "/api/v1/ws?access_token=bad", None, None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn ws_commands_respect_scope_and_filter() {
    use crate::ws::{Outcome, handle};
    let fake = Arc::new(Fake::default());
    let s = AppState::new(fake.clone(), auth());
    fake.create_session(CreateSession::default()).await.unwrap();
    let mut filter = HashSet::new();

    let submit = r#"{"cmd":"submit","id":7,"session_id":"s1","prompt":"go"}"#;
    let Outcome(v) = handle(&s, Principal::of(Scope::Read), &mut filter, submit).await;
    assert_eq!(v["event"], "command/error");
    assert_eq!(v["id"], 7);
    assert_eq!(v["data"]["code"], "forbidden");

    let Outcome(v) = handle(&s, Principal::of(Scope::Run), &mut filter, submit).await;
    assert_eq!(v["event"], "command/ack");
    assert_eq!(v["id"], 7);

    let Outcome(v) = handle(&s, Principal::of(Scope::Run), &mut filter, r#"{"cmd":"approve","approval_id":"appr_0","approved":true}"#).await;
    assert_eq!(v["event"], "command/ack");

    // Subscribing is allowed for read tokens.
    let Outcome(v) = handle(&s, Principal::of(Scope::Read), &mut filter, r#"{"cmd":"subscribe","session_id":"s1"}"#).await;
    assert_eq!(v["event"], "command/ack");
    assert!(filter.contains("s1"));

    let Outcome(v) = handle(&s, Principal::of(Scope::Run), &mut filter, "not json").await;
    assert_eq!(v["data"]["code"], "bad_request");
    let Outcome(v) = handle(&s, Principal::of(Scope::Run), &mut filter, r#"{"cmd":"explode"}"#).await;
    assert_eq!(v["data"]["code"], "bad_request");

    let calls = fake.calls();
    assert!(calls.contains(&"submit s1 go None None".to_owned()));
    assert!(calls.contains(&"approve appr_0 true".to_owned()));
    assert_eq!(calls.iter().filter(|c| c.starts_with("submit")).count(), 1);
}

/// Scripted provider: every request is answered with the same short text reply.
struct Echo;

#[async_trait::async_trait]
impl Provider for Echo {
    fn id(&self) -> &str {
        "echo"
    }
    async fn models(&self) -> hk_llm::Result<Vec<ModelInfo>> {
        Ok(Vec::new())
    }
    async fn stream(&self, _req: ChatRequest, _c: CancellationToken) -> hk_llm::Result<EventStream> {
        let evs = vec![
            Ok(StreamEvent::TextDelta { text: "pong".into() }),
            Ok(StreamEvent::Finish(FinishReason::Stop)),
        ];
        Ok(Box::pin(futures::stream::iter(evs)))
    }
}

#[tokio::test]
async fn local_runtime_runs_a_turn_and_feeds_events() {
    let dir = tempfile::tempdir().unwrap();
    let rt = LocalRuntime::new(LocalConfig {
        provider: Arc::new(Echo),
        sessions_dir: dir.path().join("sessions"),
        spill_dir: dir.path().join("spill"),
        default_model: "echo-1".into(),
        default_cwd: Some(dir.path().to_owned()),
        max_steps: 5,
    })
    .unwrap();
    let mut feed = rt.subscribe();

    let s = rt.create_session(CreateSession::default()).await.unwrap();
    assert_eq!(s.model.as_deref(), Some("echo-1"));
    assert_eq!(rt.list_sessions(10, 0).await.unwrap().len(), 1);
    assert!(matches!(rt.session("missing", None).await, Err(ApiError::NotFound(_))));
    assert!(matches!(rt.submit("missing", TurnRequest { prompt: "x".into(), mode: None, model: None }).await, Err(ApiError::NotFound(_))));

    rt.submit(&s.id, TurnRequest { prompt: "ping".into(), mode: Some(TurnMode::Plan), model: None })
        .await
        .unwrap();

    let mut names = Vec::new();
    let end = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let ev = feed.recv().await.unwrap();
            assert_eq!(ev.session_id, s.id);
            names.push(ev.name.clone());
            if ev.name == "turn/end" {
                return ev;
            }
        }
    })
    .await
    .expect("turn/end within 10s");
    assert_eq!(end.data["type"], "turn/end");
    assert!(names.iter().any(|n| n == "user/message"), "{names:?}");
    assert!(names.iter().any(|n| n == "assistant/message"), "{names:?}");
    assert!(names.iter().any(|n| n == "agent/assistant-stream"), "{names:?}");

    let d = rt.session(&s.id, None).await.unwrap();
    assert!(d.messages.iter().any(|m| format!("{m:?}").contains("pong")));
    let last = d.events.last().unwrap().seq;
    assert!(rt.session(&s.id, Some(last)).await.unwrap().events.is_empty());
    assert!(matches!(
        rt.respond_approval("appr_0", ApprovalDecision { approved: true, reason: None, session_id: None }).await,
        Err(ApiError::NotFound(_))
    ));
}
