//! End to end through the daemon's own assembly (`hk_daemon::assemble`): workflows are created
//! over REST, a webhook starts a real `Executor` run, executions land in the REST-visible
//! table. `tower::oneshot`, no sockets.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use hk_daemon::{Assembled, FlowConfig, Unconfigured, assemble, open_with};
use hk_server::{LocalConfig, LocalRuntime, router};
use hk_store::{MemorySecretStore, Store};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const ADMIN: &str = "e2e-admin-token-0123456789abcdef";

struct Rig {
    app: axum::Router,
    asm: Assembled,
    _dir: tempfile::TempDir,
}

/// Boot a daemon over `store` (a second call with the same store = a restart).
async fn boot(store: Store, secrets: Arc<MemorySecretStore>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let runtime = LocalRuntime::new(LocalConfig {
        provider: Arc::new(Unconfigured),
        sessions_dir: dir.path().join("sessions"),
        spill_dir: dir.path().join("spill"),
        default_model: "m".into(),
        default_cwd: None,
        max_steps: 1,
    })
    .unwrap();
    let opened = open_with(store, secrets, Some(ADMIN)).await.unwrap();
    let cfg = FlowConfig { webhook_base_url: Some("http://hk.test".into()), ..FlowConfig::default() };
    let asm = assemble(opened, Arc::new(runtime), cfg, "http://hk.test").await.unwrap();
    Rig { app: router(asm.state.clone()), asm, _dir: dir }
}

async fn fresh() -> (Store, Arc<MemorySecretStore>) {
    (Store::open_in_memory().await.unwrap(), Arc::new(MemorySecretStore::default()))
}

async fn call(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let st = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (st, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn api(app: &axum::Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let b = Request::builder().method(method).uri(uri).header(header::AUTHORIZATION, format!("Bearer {ADMIN}"));
    let req = match body {
        Some(v) => b.header(header::CONTENT_TYPE, "application/json").body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    }
    .unwrap();
    call(app, req).await
}

fn hook(path: &str, mode: &str) -> Value {
    json!({"id":"n1","name":"Webhook","type":"n8n-nodes-base.webhook","typeVersion":2,"position":[0,0],
           "webhookId":"wh-1","parameters":{"path":path,"httpMethod":"POST","responseMode":mode}})
}

fn set(name: &str, x: i64) -> Value {
    json!({"id":"n2","name":name,"type":"n8n-nodes-base.set","typeVersion":3.4,"position":[220,0],
           "parameters":{"mode":"manual","includeOtherFields":false,"options":{},
             "assignments":{"assignments":[
               {"id":"a1","name":"greeting","value":"={{ 'hi ' + $json.body.name }}","type":"string"},
               {"id":"a2","name":"n","value":x,"type":"number"}]}}})
}

fn chain(nodes: Vec<Value>) -> Value {
    let mut conns = serde_json::Map::new();
    for w in nodes.windows(2) {
        conns.insert(
            w[0]["name"].as_str().unwrap().into(),
            json!({"main":[[{"node": w[1]["name"], "type":"main","index":0}]]}),
        );
    }
    json!({"name":"e2e","nodes":nodes,"connections":conns,"settings":{}})
}

fn post(uri: &str, body: &str, extra: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::builder().method(Method::POST).uri(uri).header(header::CONTENT_TYPE, "application/json");
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    b.body(Body::from(body.to_owned())).unwrap()
}

async fn create_and_activate(app: &axum::Router, wf: Value) -> String {
    let (st, v) = api(app, Method::POST, "/api/v1/workflows", Some(wf)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let id = v["id"].as_str().unwrap().to_owned();
    let (st, v) = api(app, Method::POST, &format!("/api/v1/workflows/{id}/activate"), None).await;
    assert_eq!((st, &v["active"]), (StatusCode::OK, &json!(true)), "{v}");
    id
}

async fn executions(app: &axum::Router) -> Vec<Value> {
    let (st, v) = api(app, Method::GET, "/api/v1/executions?includeData=true", None).await;
    assert_eq!(st, StatusCode::OK);
    v["data"].as_array().unwrap().clone()
}

/// Background runs (onReceived, resumes) finish after the HTTP answer; poll briefly.
async fn wait_for_status(app: &axum::Router, status: &str) -> Value {
    for _ in 0..100 {
        if let Some(e) = executions(app).await.into_iter().find(|e| e["status"] == status) {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no execution reached `{status}`: {:?}", executions(app).await);
}

#[tokio::test]
async fn webhook_runs_flow_and_records_execution() {
    let (store, secrets) = fresh().await;
    let r = boot(store, secrets).await;
    let wf = chain(vec![hook("hello", "lastNode"), set("Set", 7)]);
    let id = create_and_activate(&r.app, wf).await;

    // wrong method / unknown path are plain 404s
    let get = Request::builder().uri("/webhook/hello").body(Body::empty()).unwrap();
    assert_eq!(call(&r.app, get).await.0, StatusCode::NOT_FOUND);

    let (st, v) = call(
        &r.app,
        post("/webhook/hello", r#"{"name":"ada"}"#, &[("authorization", "Bearer caller-secret-xyz"), ("x-trace", "t1")]),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"greeting":"hi ada","n":7}));

    let ex = executions(&r.app).await;
    assert_eq!(ex.len(), 1);
    assert_eq!((ex[0]["status"].as_str(), ex[0]["finished"].as_bool(), ex[0]["mode"].as_str()), (Some("success"), Some(true), Some("webhook")));
    assert_eq!(ex[0]["workflowId"], id);
    let run = &ex[0]["data"]["resultData"]["runData"];
    assert_eq!(run["Set"][0]["data"]["main"][0][0]["json"], json!({"greeting":"hi ada","n":7}));
    assert_eq!(run["Webhook"][0]["data"]["main"][0][0]["json"]["body"], json!({"name":"ada"}));
    assert_eq!(run["Webhook"][0]["data"]["main"][0][0]["json"]["webhookUrl"], "http://hk.test/webhook/hello");
    // caller credentials reach nodes but never the stored execution
    let stored = ex[0].to_string();
    assert!(!stored.contains("caller-secret-xyz"), "bearer leaked into execution data");
    assert!(stored.contains("[redacted]") && stored.contains("t1"));

    // deactivate unmounts the route
    let (st, _) = api(&r.app, Method::POST, &format!("/api/v1/workflows/{id}/deactivate"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(call(&r.app, post("/webhook/hello", "{}", &[])).await.0, StatusCode::NOT_FOUND);
    r.asm.flow.shutdown();
}

#[tokio::test]
async fn on_received_answers_immediately_and_flow_failure_is_recorded() {
    let (store, secrets) = fresh().await;
    let r = boot(store, secrets).await;
    // `$json.body.name.x` on a missing name throws inside the Set expression
    let mut bad = set("Boom", 1);
    bad["parameters"]["assignments"]["assignments"][0]["value"] = json!("={{ $json.body.nope.x }}");
    create_and_activate(&r.app, chain(vec![hook("fire", "onReceived"), bad])).await;

    let (st, v) = call(&r.app, post("/webhook/fire", r#"{"name":"ada"}"#, &[])).await;
    assert_eq!((st, v), (StatusCode::OK, json!({"message":"Workflow was started"})));
    let e = wait_for_status(&r.app, "error").await;
    assert_eq!(e["finished"], false);
    assert!(e["data"]["resultData"]["error"]["message"].is_string());
    r.asm.flow.shutdown();
}

#[tokio::test]
async fn parked_wait_survives_restart_and_resumes() {
    let (store, secrets) = fresh().await;
    let r = boot(store.clone(), secrets.clone()).await;
    let wait = json!({"id":"w","name":"Pause","type":"n8n-nodes-base.wait","typeVersion":1.1,"position":[110,0],
                      "parameters":{"resume":"timeInterval","amount":3,"unit":"hours"}});
    let id = create_and_activate(&r.app, chain(vec![hook("later", "onReceived"), wait, set("After", 1)])).await;

    let (st, _) = call(&r.app, post("/webhook/later", r#"{"name":"bob"}"#, &[])).await;
    assert_eq!(st, StatusCode::OK);
    let e = wait_for_status(&r.app, "waiting").await;
    assert_eq!(e["finished"], false);
    assert_eq!(store.list_waits().await.unwrap().len(), 1, "snapshot parked in sqlite");
    // nothing due yet
    assert_eq!(r.asm.flow.resume_due(0).await, 0);

    // "restart": new runtime over the same db; stored active workflow re-registers its webhook
    r.asm.flow.shutdown();
    drop(r);
    let r2 = boot(store.clone(), secrets).await;
    assert_eq!(r2.asm.flow.host().active_ids(), vec![id.clone()]);
    assert_eq!(r2.asm.flow.resume_due(0).await, 0);

    let in_four_hours = i64::try_from(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
    )
    .unwrap()
        + 4 * 3600 * 1000;
    assert_eq!(r2.asm.flow.resume_due(in_four_hours).await, 1);
    let done = wait_for_status(&r2.app, "success").await;
    assert_eq!(done["finished"], true);
    assert_eq!(done["data"]["resultData"]["runData"]["After"][0]["data"]["main"][0][0]["json"], json!({"greeting":"hi bob","n":1}));
    assert!(store.list_waits().await.unwrap().is_empty(), "claimed wait is gone");
    r2.asm.flow.shutdown();
}

#[tokio::test]
async fn pairing_qr_endpoint_and_exchange() {
    let (store, secrets) = fresh().await;
    let r = boot(store, secrets).await;
    let (st, v) = api(&r.app, Method::GET, "/api/v1/pairing?scope=run", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["url"], "http://hk.test");
    assert!(v["qr_svg"].as_str().unwrap().contains("<svg"));
    let tok = v["token"].as_str().unwrap();
    assert!(tok.starts_with("hkp_run."));

    let ex = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/pairing/exchange")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token": tok}).to_string()))
        .unwrap();
    let (st, v) = call(&r.app, ex).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let run_token = v["token"].as_str().unwrap();
    assert_ne!(run_token, ADMIN, "exchange yields the requested tier, not admin");
    // and that token really is a run-tier token
    let req = Request::builder().uri("/api/v1/models").header(header::AUTHORIZATION, format!("Bearer {run_token}")).body(Body::empty()).unwrap();
    assert_ne!(call(&r.app, req).await.0, StatusCode::UNAUTHORIZED);
    r.asm.flow.shutdown();
}
