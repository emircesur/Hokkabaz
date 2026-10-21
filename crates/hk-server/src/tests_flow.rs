//! n8n-shaped surface: webhooks, REST v1, scopes, rate limit, OpenAPI. All through
//! `tower::oneshot`; no sockets.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::FutureExt;
use hk_flow::hooks::{ResponseBody, WebhookResponse};
use hk_flow::Item;
use hk_store::{MemorySecretStore, Secret, SecretStore, Store};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tower::ServiceExt;

use crate::tests::Fake;
use crate::*;

const WF_R: &str = "t-wf-read";
const WF_W: &str = "t-wf-write";
const EX_R: &str = "t-ex-read";
const CRED: &str = "t-cred";
const HOOK: &str = "t-hook";
const ADMIN: &str = "t-admin";

struct Rig {
    app: axum::Router,
    flow: FlowApi,
    registry: WebhookRegistry,
    secrets: Arc<MemorySecretStore>,
    seen: Arc<Mutex<Vec<Value>>>,
}

fn auth() -> Auth {
    Auth::new()
        .with_token(Scope::WorkflowRead, WF_R)
        .with_token(Scope::WorkflowWrite, WF_W)
        .with_token(Scope::ExecutionRead, EX_R)
        .with_token(Scope::CredentialWrite, CRED)
        .with_token(Scope::WebhookInvoke, HOOK)
        .with_token(Scope::Admin, ADMIN)
}

async fn rig_with(limit: Option<RateLimit>) -> Rig {
    let store = Store::open_in_memory().await.unwrap();
    let secrets = Arc::new(MemorySecretStore::default());
    let registry = WebhookRegistry::new();
    let flow = FlowApi::new(store, secrets.clone()).with_webhooks(Arc::new(registry.clone()));
    let state = AppState::new(Arc::new(Fake::default()), auth()).with_flow(flow.clone()).with_rate_limit(limit);
    Rig { app: router(state), flow, registry, secrets, seen: Arc::default() }
}

async fn rig() -> Rig {
    rig_with(None).await
}

impl Rig {
    /// Webhook whose target records the item and finishes with `out` (or errors).
    fn hook(&self, mode: WebhookMode, spec: WebhookSpec, out: Result<Vec<Item>, String>, ttl: Option<Duration>) {
        let seen = self.seen.clone();
        let out = Arc::new(Mutex::new(Some(out)));
        let target: WebhookTarget = Arc::new(move |call: WebhookCall| {
            let seen = seen.clone();
            let out = out.clone();
            async move {
                seen.lock().unwrap().push(serde_json::to_value(&call.item).unwrap());
                let (tx, rx) = oneshot::channel();
                // responseNode mode: a "Respond to Webhook" node answers first.
                if call.path == "respond" {
                    call.sink.respond(WebhookResponse {
                        status: 201,
                        headers: vec![("x-custom".into(), "yes".into())],
                        body: ResponseBody::Json(json!({"from":"node"})),
                    });
                }
                let _ = tx.send(out.lock().unwrap().take().unwrap_or(Ok(vec![])));
                Ok(WebhookRun { execution_id: Some(1), done: rx })
            }
            .boxed()
        });
        self.registry.register(mode, spec, ttl, target).unwrap();
    }
}

fn spec(path: &str, methods: &[&str]) -> WebhookSpec {
    let mut s = WebhookSpec::new("wf1", path);
    s.methods = methods.iter().map(|m| (*m).to_owned()).collect();
    s
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let (st, h) = (res.status(), res.headers().clone());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (st, h, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn api(app: &axum::Router, method: Method, uri: &str, tok: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = tok {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(v) => b.header(header::CONTENT_TYPE, "application/json").body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    }
    .unwrap();
    let (s, _, v) = send(app, req).await;
    (s, v)
}

fn post(uri: &str, ct: &str, body: impl Into<Body>) -> Request<Body> {
    Request::builder().method(Method::POST).uri(uri).header(header::CONTENT_TYPE, ct).body(body.into()).unwrap()
}

// ---------------------------------------------------------------- webhooks

#[tokio::test]
async fn webhook_builds_item_and_routes_by_method_and_params() {
    let r = rig().await;
    r.hook(WebhookMode::Production, spec("orders/:id", &["post"]), Ok(vec![]), None);

    let (st, _, v) = send(&r.app, post("/webhook/orders/42?a=1&a=2&b=x", "application/json", r#"{"k":[1]}"#)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v, json!({"message":"Workflow was started"}));

    let item = r.seen.lock().unwrap()[0].clone();
    assert_eq!(item["json"]["body"], json!({"k":[1]}));
    assert_eq!(item["json"]["params"], json!({"id":"42"}));
    assert_eq!(item["json"]["query"], json!({"a":["1","2"],"b":"x"}));
    assert_eq!(item["json"]["headers"]["content-type"], "application/json");

    // wrong method, wrong path, test prefix, bad json
    let get = Request::builder().uri("/webhook/orders/42").body(Body::empty()).unwrap();
    assert_eq!(send(&r.app, get).await.0, StatusCode::NOT_FOUND);
    let (st, _, v) = send(&r.app, post("/webhook/nope", "application/json", "{}")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(v["message"].as_str().unwrap().contains("POST nope"));
    assert_eq!(send(&r.app, post("/webhook-test/orders/42", "application/json", "{}")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(send(&r.app, post("/webhook/orders/1", "application/json", "{")).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(r.seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn webhook_form_body_and_literal_beats_param() {
    let r = rig().await;
    r.hook(WebhookMode::Production, spec("a/:x", &["POST"]), Ok(vec![]), None);
    r.hook(WebhookMode::Production, spec("a/fixed", &["POST"]), Ok(vec![]), None);
    send(&r.app, post("/webhook/a/fixed", "application/x-www-form-urlencoded", "u=1&v=two%20words")).await;
    let item = r.seen.lock().unwrap()[0].clone();
    assert_eq!(item["json"]["body"], json!({"u":"1","v":"two words"}));
    assert_eq!(item["json"]["params"], json!({}), "literal route won, no capture");
    // conflict on re-register
    let dup: WebhookTarget = Arc::new(|_| async { Err(ApiError::Internal("x".into())) }.boxed());
    assert!(matches!(
        r.registry.register(WebhookMode::Production, spec("a/:x", &["post"]), None, dup),
        Err(ApiError::Conflict(_))
    ));
}

#[tokio::test]
async fn test_listener_is_one_shot_and_survives_rejected_calls() {
    let r = rig().await;
    let mut s = spec("t", &["POST"]);
    s.hmac = Some(HmacSpec { preset: HmacPreset::Github, secret: "wh".into() });
    r.secrets.set("wh", &Secret::new("k")).unwrap();
    r.hook(WebhookMode::Test, s, Ok(vec![]), None);

    // unsigned call is rejected and does not consume the listener
    assert_eq!(send(&r.app, post("/webhook-test/t", "application/json", "{}")).await.0, StatusCode::UNAUTHORIZED);
    let body = "{}";
    let sig = crate::signature::sign_hex(b"k", body.as_bytes());
    let signed = || {
        Request::builder()
            .method(Method::POST)
            .uri("/webhook-test/t")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-hub-signature-256", format!("sha256={sig}"))
            .body(Body::from(body))
            .unwrap()
    };
    assert_eq!(send(&r.app, signed()).await.0, StatusCode::OK);
    let (st, _, v) = send(&r.app, signed()).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(v["hint"].as_str().unwrap().contains("Execute workflow"));
    assert_eq!(r.seen.lock().unwrap().len(), 1);
    assert!(r.registry.is_empty(), "one-shot entry removed");
}

#[tokio::test]
async fn test_listener_expires() {
    let r = rig().await;
    r.hook(WebhookMode::Test, spec("e", &["POST"]), Ok(vec![]), Some(Duration::from_millis(1)));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(send(&r.app, post("/webhook-test/e", "application/json", "{}")).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn hmac_missing_secret_fails_closed_without_naming_it() {
    let r = rig().await;
    let mut s = spec("h", &["POST"]);
    s.hmac = Some(HmacSpec { preset: HmacPreset::Github, secret: "absent-handle".into() });
    r.hook(WebhookMode::Production, s, Ok(vec![]), None);
    let (st, _, v) = send(&r.app, post("/webhook/h", "application/json", "{}")).await;
    assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!v.to_string().contains("absent-handle"));
    assert!(r.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn body_limit_enforced_by_header_and_by_stream() {
    let r = rig().await;
    let mut s = spec("big", &["POST"]);
    s.max_body_bytes = 8;
    r.hook(WebhookMode::Production, s, Ok(vec![]), None);
    assert_eq!(send(&r.app, post("/webhook/big", "text/plain", "12345678")).await.0, StatusCode::OK);
    let (st, _, v) = send(&r.app, post("/webhook/big", "text/plain", "123456789")).await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(v["code"], 413);
    // no Content-Length (chunked): the reader still caps
    let chunks = futures::stream::iter(vec![Ok::<_, std::io::Error>("12345".as_bytes()), Ok("67890".as_bytes())]);
    let req = Request::builder()
        .method(Method::POST)
        .uri("/webhook/big")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from_stream(chunks))
        .unwrap();
    assert_eq!(send(&r.app, req).await.0, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn last_node_response_modes() {
    let r = rig().await;
    let items = vec![Item::from_json(json!({"n":1})), Item::from_json(json!({"n":2}))];
    let mut s1 = spec("first", &["POST"]);
    s1.response_mode = ResponseMode::LastNode;
    r.hook(WebhookMode::Production, s1, Ok(items.clone()), None);
    let mut s2 = spec("all", &["POST"]);
    s2.response_mode = ResponseMode::LastNode;
    s2.response_data = LastNodeData::AllEntries;
    s2.response_code = 202;
    r.hook(WebhookMode::Production, s2, Ok(items), None);
    let mut s3 = spec("boom", &["POST"]);
    s3.response_mode = ResponseMode::LastNode;
    r.hook(WebhookMode::Production, s3, Err("secret stack trace".into()), None);

    let (st, _, v) = send(&r.app, post("/webhook/first", "application/json", "{}")).await;
    assert_eq!((st, v), (StatusCode::OK, json!({"n":1})));
    let (st, _, v) = send(&r.app, post("/webhook/all", "application/json", "{}")).await;
    assert_eq!((st, v), (StatusCode::ACCEPTED, json!([{"n":1},{"n":2}])));
    let (st, _, v) = send(&r.app, post("/webhook/boom", "application/json", "{}")).await;
    assert_eq!(st, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!v.to_string().contains("stack"), "workflow error text is not echoed");
}

#[tokio::test]
async fn response_node_mode_maps_node_answer_and_falls_back() {
    let r = rig().await;
    let mut s = spec("respond", &["POST"]);
    s.response_mode = ResponseMode::ResponseNode;
    r.hook(WebhookMode::Production, s, Ok(vec![]), None);
    let (st, h, v) = send(&r.app, post("/webhook/respond", "application/json", "{}")).await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(h["x-custom"], "yes");
    assert_eq!(h[header::CONTENT_TYPE], "application/json");
    assert_eq!(v, json!({"from":"node"}));

    // workflow ends without a Respond to Webhook node
    let mut s = spec("silent", &["POST"]);
    s.response_mode = ResponseMode::ResponseNode;
    r.hook(WebhookMode::Production, s, Ok(vec![]), None);
    let (st, _, v) = send(&r.app, post("/webhook/silent", "application/json", "{}")).await;
    assert_eq!((st, v), (StatusCode::OK, json!({"message":"Workflow executed successfully"})));
}

#[tokio::test]
async fn bearer_webhook_needs_webhook_invoke() {
    let r = rig().await;
    let mut s = spec("priv", &["POST"]);
    s.auth = WebhookAuth::Bearer;
    r.hook(WebhookMode::Production, s, Ok(vec![]), None);
    let with = |tok: Option<&str>| {
        let mut b = Request::builder().method(Method::POST).uri("/webhook/priv");
        if let Some(t) = tok {
            b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        b.body(Body::empty()).unwrap()
    };
    assert_eq!(send(&r.app, with(None)).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&r.app, with(Some(WF_R))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(send(&r.app, with(Some(HOOK))).await.0, StatusCode::OK);
    assert_eq!(send(&r.app, with(Some(ADMIN))).await.0, StatusCode::OK);
}

#[tokio::test]
async fn flow_routes_absent_without_flow_api() {
    let app = router(AppState::new(Arc::new(Fake::default()), auth()));
    assert_eq!(send(&app, post("/webhook/x", "application/json", "{}")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(api(&app, Method::GET, "/api/v1/workflows", Some(ADMIN), None).await.0, StatusCode::NOT_FOUND);
    // openapi is always served
    assert_eq!(api(&app, Method::GET, "/api/v1/openapi.json", None, None).await.0, StatusCode::OK);
}

// ---------------------------------------------------------------- REST v1

fn wf(name: &str) -> Value {
    json!({
        "name": name,
        "nodes": [{"id":"1","name":"Webhook","type":"n8n-nodes-base.webhook","typeVersion":2,"position":[0,0],"parameters":{"path":"x"}}],
        "connections": {},
        "settings": {"executionOrder":"v1"},
        "custom": {"keep": true}
    })
}

#[tokio::test]
async fn workflow_crud_activate_and_scopes() {
    let r = rig().await;
    let a = &r.app;
    // auth / scope gates
    assert_eq!(api(a, Method::GET, "/api/v1/workflows", None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(api(a, Method::GET, "/api/v1/workflows", Some(EX_R), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(api(a, Method::POST, "/api/v1/workflows", Some(WF_R), Some(wf("a"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(api(a, Method::POST, "/api/v1/workflows", Some(CRED), Some(wf("a"))).await.0, StatusCode::FORBIDDEN);

    let (st, created) = api(a, Method::POST, "/api/v1/workflows", Some(WF_W), Some(wf("Alpha"))).await;
    assert_eq!(st, StatusCode::OK, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["active"], false);
    assert_eq!(created["custom"], json!({"keep":true}), "unknown fields survive");
    assert!(created["createdAt"].as_str().unwrap().ends_with('Z'));
    assert!(created["versionId"].is_string());

    assert_eq!(api(a, Method::POST, "/api/v1/workflows", Some(WF_W), Some(json!({"name":"x"}))).await.0, StatusCode::BAD_REQUEST);

    let (_, list) = api(a, Method::GET, "/api/v1/workflows?name=alp&limit=1", Some(WF_R), None).await;
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert_eq!(list["nextCursor"], Value::Null);
    api(a, Method::POST, "/api/v1/workflows", Some(WF_W), Some(wf("Beta"))).await;
    let (_, p1) = api(a, Method::GET, "/api/v1/workflows?limit=1", Some(WF_R), None).await;
    let cur = p1["nextCursor"].as_str().expect("second page exists").to_owned();
    let (_, p2) = api(a, Method::GET, &format!("/api/v1/workflows?limit=1&cursor={cur}"), Some(WF_R), None).await;
    assert_ne!(p1["data"][0]["id"], p2["data"][0]["id"]);
    assert_eq!(api(a, Method::GET, "/api/v1/workflows?cursor=zz", Some(WF_R), None).await.0, StatusCode::BAD_REQUEST);

    // n8n header works too
    let req = Request::builder().uri(format!("/api/v1/workflows/{id}")).header("x-n8n-api-key", WF_R).body(Body::empty()).unwrap();
    assert_eq!(send(a, req).await.0, StatusCode::OK);

    let (st, upd) = api(a, Method::PUT, &format!("/api/v1/workflows/{id}"), Some(WF_W), Some(wf("Alpha2"))).await;
    assert_eq!((st, upd["name"].as_str()), (StatusCode::OK, Some("Alpha2")));
    assert_ne!(upd["versionId"], created["versionId"]);

    let (_, act) = api(a, Method::POST, &format!("/api/v1/workflows/{id}/activate"), Some(WF_W), None).await;
    assert_eq!(act["active"], true);
    let (_, only_active) = api(a, Method::GET, "/api/v1/workflows?active=true", Some(WF_R), None).await;
    assert_eq!(only_active["data"].as_array().unwrap().len(), 1);
    let (_, deact) = api(a, Method::POST, &format!("/api/v1/workflows/{id}/deactivate"), Some(WF_W), None).await;
    assert_eq!(deact["active"], false);

    assert_eq!(api(a, Method::DELETE, &format!("/api/v1/workflows/{id}"), Some(WF_W), None).await.0, StatusCode::OK);
    assert_eq!(api(a, Method::GET, &format!("/api/v1/workflows/{id}"), Some(WF_R), None).await.0, StatusCode::NOT_FOUND);
}

#[derive(Debug, Default)]
struct RecordingActivator {
    calls: Mutex<Vec<String>>,
    fail: bool,
}

#[async_trait::async_trait]
impl WorkflowActivator for RecordingActivator {
    async fn activate(&self, w: &Value) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("on {}", w["id"].as_str().unwrap()));
        if self.fail { Err("no trigger node".into()) } else { Ok(()) }
    }
    async fn deactivate(&self, id: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("off {id}"));
        Ok(())
    }
}

#[tokio::test]
async fn activator_failure_is_400_and_leaves_inactive() {
    let store = Store::open_in_memory().await.unwrap();
    let act = Arc::new(RecordingActivator { fail: true, ..Default::default() });
    let flow = FlowApi::new(store, Arc::new(MemorySecretStore::default())).with_activator(act.clone());
    let app = router(AppState::new(Arc::new(Fake::default()), auth()).with_flow(flow).with_rate_limit(None));
    let (_, c) = api(&app, Method::POST, "/api/v1/workflows", Some(ADMIN), Some(wf("w"))).await;
    let id = c["id"].as_str().unwrap();
    let (st, v) = api(&app, Method::POST, &format!("/api/v1/workflows/{id}/activate"), Some(ADMIN), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"]["message"].as_str().unwrap().contains("no trigger node"));
    let (_, g) = api(&app, Method::GET, &format!("/api/v1/workflows/{id}"), Some(ADMIN), None).await;
    assert_eq!(g["active"], false);
    assert_eq!(act.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn executions_list_get_delete_and_events() {
    let r = rig().await;
    let a = &r.app;
    let (_, c) = api(a, Method::POST, "/api/v1/workflows", Some(WF_W), Some(wf("w"))).await;
    let wid = c["id"].as_str().unwrap();

    let mut ev = r.flow.subscribe();
    let e1 = r.flow.start_execution(wid, "webhook", None).await.unwrap();
    let e2 = r.flow.start_execution(wid, "webhook", None).await.unwrap();
    let data = json!({"resultData":{"runData":{"Webhook":[{"data":{"main":[[{"json":{"a":1}}]]}}]}}});
    r.flow.finish_execution(e1, "success", Some(&data)).await.unwrap();
    r.flow.finish_execution(e2, "error", None).await.unwrap();

    let started = ev.recv().await.unwrap();
    assert_eq!((started.name.as_str(), started.session_id.as_str()), ("execution/started", e1.to_string().as_str()));
    assert_eq!(started.data["workflow_id"], wid);
    ev.recv().await.unwrap();
    let fin = ev.recv().await.unwrap();
    assert_eq!(fin.name, "execution/finished");
    assert_eq!(fin.data["status"], "success");

    assert_eq!(api(a, Method::GET, "/api/v1/executions", Some(WF_R), None).await.0, StatusCode::FORBIDDEN);
    let (_, l) = api(a, Method::GET, "/api/v1/executions?limit=1", Some(EX_R), None).await;
    assert_eq!(l["data"][0]["id"], e2);
    assert!(l["data"][0].get("data").is_none(), "no data unless asked");
    assert_eq!(l["nextCursor"], e2.to_string());
    let (_, l2) = api(a, Method::GET, &format!("/api/v1/executions?limit=1&cursor={e2}&includeData=true"), Some(EX_R), None).await;
    assert_eq!(l2["data"][0]["id"], e1);
    assert_eq!(l2["data"][0]["data"], data);
    assert_eq!(l2["nextCursor"], Value::Null);
    let (_, ok) = api(a, Method::GET, &format!("/api/v1/executions?status=success&workflowId={wid}"), Some(EX_R), None).await;
    assert_eq!(ok["data"].as_array().unwrap().len(), 1);
    assert_eq!(api(a, Method::GET, "/api/v1/executions?status=bogus", Some(EX_R), None).await.0, StatusCode::BAD_REQUEST);

    let (st, g) = api(a, Method::GET, &format!("/api/v1/executions/{e1}"), Some(EX_R), None).await;
    assert_eq!((st, g["finished"].clone(), g["status"].clone()), (StatusCode::OK, json!(true), json!("success")));
    assert_eq!(api(a, Method::DELETE, &format!("/api/v1/executions/{e1}"), Some(EX_R), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(api(a, Method::DELETE, &format!("/api/v1/executions/{e1}"), Some(WF_W), None).await.0, StatusCode::OK);
    assert_eq!(api(a, Method::GET, &format!("/api/v1/executions/{e1}"), Some(EX_R), None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn credentials_are_write_only_and_secret_backed() {
    let r = rig().await;
    let a = &r.app;
    let body = json!({"name":"gh","type":"httpBasicAuth","data":{"user":"me","password":"hunter2"}});
    assert_eq!(api(a, Method::POST, "/api/v1/credentials", Some(WF_W), Some(body.clone())).await.0, StatusCode::FORBIDDEN);
    let (st, c) = api(a, Method::POST, "/api/v1/credentials", Some(CRED), Some(body)).await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert!(!c.to_string().contains("hunter2"));
    assert_eq!((c["name"].clone(), c["type"].clone()), (json!("gh"), json!("httpBasicAuth")));
    let id = c["id"].as_str().unwrap();

    // value is in the secret store, only names in sqlite
    let stored = r.secrets.get(&format!("credential.{id}")).unwrap().unwrap();
    assert!(stored.expose().contains("hunter2"));
    assert_eq!(r.flow.store.list_secret_names().await.unwrap(), vec![format!("credential.{id}")]);
    let dump = format!("{:?}", r.flow.store.get_credential(id).await.unwrap());
    assert!(!dump.contains("hunter2"));

    // validation never echoes values
    let (st, v) = api(a, Method::POST, "/api/v1/credentials", Some(CRED),
        Some(json!({"name":"x","type":"httpBasicAuth","data":{"user":"u","password":"p","evil":"leak-me"}}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(!v.to_string().contains("leak-me"));
    assert_eq!(api(a, Method::POST, "/api/v1/credentials", Some(CRED),
        Some(json!({"name":"x","type":"nopeApi","data":{}}))).await.0, StatusCode::BAD_REQUEST);

    let (st, s) = api(a, Method::GET, "/api/v1/credentials/schema/httpBasicAuth", Some(CRED), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(s["required"], json!(["user","password"]));
    assert_eq!(api(a, Method::GET, "/api/v1/credentials/schema/zzz", Some(CRED), None).await.0, StatusCode::NOT_FOUND);

    let (st, d) = api(a, Method::DELETE, &format!("/api/v1/credentials/{id}"), Some(CRED), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!d.to_string().contains("hunter2"));
    assert!(r.secrets.get(&format!("credential.{id}")).unwrap().is_none());
    assert_eq!(api(a, Method::DELETE, &format!("/api/v1/credentials/{id}"), Some(CRED), None).await.0, StatusCode::NOT_FOUND);
    // there is no read route for credentials
    assert_eq!(api(a, Method::GET, &format!("/api/v1/credentials/{id}"), Some(ADMIN), None).await.0, StatusCode::METHOD_NOT_ALLOWED);
}

// ----------------------------------------------------- rate limit + openapi

#[tokio::test]
async fn rate_limit_is_per_token_and_after_auth() {
    let r = rig_with(Some(RateLimit { burst: 2, per_sec: 0.01 })).await;
    let a = &r.app;
    for _ in 0..2 {
        assert_eq!(api(a, Method::GET, "/api/v1/workflows", Some(WF_R), None).await.0, StatusCode::OK);
    }
    let req = Request::builder().uri("/api/v1/workflows").header(header::AUTHORIZATION, format!("Bearer {WF_R}")).body(Body::empty()).unwrap();
    let (st, h, v) = send(a, req).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert!(h.contains_key(header::RETRY_AFTER));
    assert_eq!(v["error"]["code"], "rate_limited");
    // other tokens unaffected; bad tokens are 401 and don't touch buckets
    assert_eq!(api(a, Method::GET, "/api/v1/executions", Some(EX_R), None).await.0, StatusCode::OK);
    assert_eq!(api(a, Method::GET, "/api/v1/workflows", Some("bad"), None).await.0, StatusCode::UNAUTHORIZED);
    // health stays open
    assert_eq!(api(a, Method::GET, "/api/v1/health", None, None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn openapi_document_lists_the_surface() {
    let r = rig().await;
    let (st, doc) = api(&r.app, Method::GET, "/api/v1/openapi.json", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(doc["openapi"].as_str().unwrap().starts_with("3."));
    let paths = doc["paths"].as_object().unwrap();
    for p in [
        "/api/v1/workflows",
        "/api/v1/workflows/{id}",
        "/api/v1/workflows/{id}/activate",
        "/api/v1/workflows/{id}/deactivate",
        "/api/v1/executions",
        "/api/v1/executions/{id}",
        "/api/v1/credentials",
        "/api/v1/credentials/{id}",
        "/api/v1/credentials/schema/{credentialTypeName}",
        "/webhook/{path}",
        "/webhook-test/{path}",
    ] {
        assert!(paths.contains_key(p), "missing {p}");
    }
    assert!(doc["components"]["securitySchemes"]["bearer"].is_object());
    assert!(doc["components"]["securitySchemes"]["n8nApiKey"].is_object());
    // no credential read operation is documented
    assert!(paths["/api/v1/credentials/{id}"].get("get").is_none());
}
