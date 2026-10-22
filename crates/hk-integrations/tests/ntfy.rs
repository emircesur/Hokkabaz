//! ntfy demo integration against a local server, through the public registration entry point.
#![cfg(feature = "int-ntfy")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::{CredentialTypes, register_integrations};
use serde_json::{Value, json};

use common::{Creds, http_resp, serve};

fn setup() -> (NodeRegistry, CredentialTypes) {
    let mut nodes = NodeRegistry::new();
    let creds = CredentialTypes::new();
    register_integrations(&mut nodes, &creds).unwrap();
    (nodes, creds)
}

fn ctx(params: Value, token: &str, extra: Value) -> NodeContext {
    let mut node: Node = serde_json::from_value(json!({
        "name": "Notify", "type": "hokkabaz.ntfy", "typeVersion": 1, "position": [0, 0],
        "parameters": params,
        "credentials": {"ntfyApi": {"id": "c1", "name": "ntfy"}}
    }))
    .unwrap();
    if let Some(m) = extra.as_object() {
        node.continue_on_fail = m.get("continueOnFail").and_then(Value::as_bool);
    }
    let mut c = NodeContext::new(node, vec![vec![Item::from_json(json!({"who": "me"}))]], 0);
    c.hooks.credentials = Some(Arc::new(Creds(json!({"serverUrl": token}))));
    c
}

fn with_server(mut c: NodeContext, url: &str, secret: &str) -> NodeContext {
    c.hooks.credentials = Some(Arc::new(Creds(json!({"serverUrl": url, "accessToken": secret}))));
    c
}

#[test]
fn registers_node_and_credential_schema() {
    let (nodes, creds) = setup();
    assert!(nodes.latest("hokkabaz.ntfy").is_some());
    // generic types come along
    assert!(creds.get("httpHeaderAuth").is_some());
    let s = creds.schema("ntfyApi").unwrap();
    assert_eq!(s["properties"]["serverUrl"]["default"], "https://ntfy.sh");
    assert_eq!(s["required"], json!([]));
    // idempotent for credentials, node registration is not
    let mut nodes2 = NodeRegistry::new();
    register_integrations(&mut nodes2, &creds).unwrap();
    assert!(register_integrations(&mut nodes2, &creds).is_err());
}

#[tokio::test]
async fn sends_json_publish_with_bearer_auth() {
    let (nodes, _c) = setup();
    let (url, seen) = serve(vec![http_resp("200 OK", r#"{"id":"abc","time":1,"event":"message","topic":"alerts"}"#)]).await;
    let params = json!({
        "resource": "message", "operation": "send", "topic": "alerts", "message": "disk full",
        "additionalFields": {"title": "Host down", "priority": 5, "tags": "warning, skull"}
    });
    let c = with_server(ctx(params, "", json!({})), &url, "tk_SECRET");
    let node = nodes.latest("hokkabaz.ntfy").unwrap();
    let out = node.execute(&c).await.unwrap();
    assert_eq!(out[0].len(), 1);
    assert_eq!(out[0][0].json["id"], "abc");

    let req = seen.lock().unwrap()[0].clone();
    assert!(req.starts_with("POST / HTTP/1.1"), "{req}");
    assert!(req.to_lowercase().contains("authorization: bearer tk_secret".to_lowercase().as_str()));
    let body: Value = serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(
        body,
        json!({"topic": "alerts", "message": "disk full", "title": "Host down", "priority": 5, "tags": ["warning", "skull"]})
    );
}

#[tokio::test]
async fn errors_are_mapped_and_never_leak_the_token() {
    let (nodes, _c) = setup();
    let node = nodes.latest("hokkabaz.ntfy").unwrap();
    let params = json!({"resource": "message", "operation": "send", "topic": "t", "message": "m"});
    let canned = http_resp("401 Unauthorized", r#"{"code":40101,"http":401,"error":"unauthorized"}"#);

    let (url, _s) = serve(vec![canned.clone()]).await;
    let c = with_server(ctx(params.clone(), "", json!({})), &url, "tk_SECRET");
    let e = node.execute(&c).await.unwrap_err().to_string();
    assert!(e.contains("401") && e.contains("unauthorized") && e.contains("credentials"), "{e}");
    assert!(!e.contains("tk_SECRET"));

    let (url, _s) = serve(vec![canned]).await;
    let c = with_server(ctx(params, "", json!({"continueOnFail": true})), &url, "tk_SECRET");
    let out = node.execute(&c).await.unwrap();
    let item = &out[0][0];
    assert!(item.json["error"].as_str().unwrap().contains("401"));
    assert!(item.error.is_some());
    assert!(!format!("{item:?}").contains("tk_SECRET"));
}

#[tokio::test]
async fn missing_topic_is_not_sent_and_server_rejects() {
    // empty required params are skipped by routing; the service reports the problem
    let (nodes, _c) = setup();
    let (url, seen) = serve(vec![http_resp("400 Bad Request", r#"{"error":"topic missing"}"#)]).await;
    let c = with_server(
        ctx(json!({"resource": "message", "operation": "send", "topic": "", "message": "m"}), "", json!({})),
        &url,
        "",
    );
    let e = nodes.latest("hokkabaz.ntfy").unwrap().execute(&c).await.unwrap_err().to_string();
    assert!(e.contains("400") && e.contains("topic missing"), "{e}");
    let req = seen.lock().unwrap()[0].clone();
    assert!(!req.contains("\"topic\""));
    // empty accessToken: no auth header
    assert!(!req.to_lowercase().contains("authorization"));
}
