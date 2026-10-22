//! Gotify node against a local server: per-operation token choice, paging, routing.
#![cfg(feature = "int-gotify")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::{CredentialTypes, register_integrations};
use serde_json::{Value, json};

use common::{Creds, http_resp, serve};

fn setup() -> NodeRegistry {
    let mut nodes = NodeRegistry::new();
    register_integrations(&mut nodes, &CredentialTypes::new()).unwrap();
    nodes
}

fn ctx(params: Value, cred: Value) -> NodeContext {
    let node: Node = serde_json::from_value(json!({
        "name": "Gotify", "type": "n8n-nodes-base.gotify", "typeVersion": 1, "position": [0, 0],
        "parameters": params,
        "credentials": {"gotifyApi": {"id": "c1", "name": "gf"}}
    }))
    .unwrap();
    let mut c = NodeContext::new(node, vec![vec![Item::from_json(json!({}))]], 0);
    c.hooks.credentials = Some(Arc::new(Creds(cred)));
    c
}

fn both(url: &str) -> Value {
    json!({"url": url, "appApiToken": "APPTOK", "clientApiToken": "CLITOK"})
}

fn key(req: &str) -> Option<String> {
    req.lines().find_map(|l| {
        l.to_lowercase()
            .strip_prefix("x-gotify-key:")
            .map(|v| v.trim().to_owned())
    })
}

fn body_of(req: &str) -> Value {
    serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

#[tokio::test]
async fn create_uses_the_app_token() {
    let nodes = setup();
    let (url, seen) = serve(vec![http_resp(
        "200 OK",
        r#"{"id":7,"appid":1,"message":"hi","priority":5}"#,
    )])
    .await;
    let params = json!({
        "resource": "message", "operation": "create", "message": "hi", "priority": 5,
        "additionalFields": {"title": "T", "contentType": "text/markdown"}
    });
    let out = nodes
        .latest("n8n-nodes-base.gotify")
        .unwrap()
        .execute(&ctx(params, both(&url)))
        .await
        .unwrap();
    assert_eq!(out[0][0].json["id"], 7);
    let req = seen.lock().unwrap()[0].clone();
    assert!(req.starts_with("POST /message HTTP/1.1"), "{req}");
    assert_eq!(key(&req).as_deref(), Some("apptok"));
    assert_eq!(
        body_of(&req),
        json!({"message": "hi", "priority": 5, "title": "T",
               "extras": {"client::display": {"contentType": "text/markdown"}}})
    );
}

#[tokio::test]
async fn get_all_follows_paging_with_the_client_token() {
    let nodes = setup();
    let n = nodes.latest("n8n-nodes-base.gotify").unwrap();
    let (url, seen) = serve(vec![
        http_resp(
            "200 OK",
            r#"{"messages":[{"id":9},{"id":8}],"paging":{"size":2,"since":8,"limit":200,"next":"http://x/message?since=8"}}"#,
        ),
        http_resp("200 OK", r#"{"messages":[{"id":7}],"paging":{"size":1,"since":0,"limit":200}}"#),
    ])
    .await;
    let params = json!({"resource": "message", "operation": "getAll", "returnAll": true});
    let out = n.execute(&ctx(params, both(&url))).await.unwrap();
    let ids: Vec<_> = out[0]
        .iter()
        .map(|i| i.json["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [9, 8, 7]);
    let reqs = seen.lock().unwrap().clone();
    assert!(
        reqs[0].starts_with("GET /message?limit=200 HTTP/1.1"),
        "{}",
        reqs[0]
    );
    assert!(reqs[1].contains("since=8"), "{}", reqs[1]);
    assert!(reqs.iter().all(|r| key(r).as_deref() == Some("clitok")));

    // limit: one request, truncated
    let (url, seen) = serve(vec![http_resp(
        "200 OK",
        r#"{"messages":[{"id":9},{"id":8}],"paging":{"size":2,"since":8,"limit":2,"next":"x"}}"#,
    )])
    .await;
    let params = json!({"resource": "message", "operation": "getAll", "limit": 1});
    let out = n.execute(&ctx(params, both(&url))).await.unwrap();
    assert_eq!(out[0].len(), 1);
    assert!(seen.lock().unwrap()[0].contains("limit=1"));
}

#[tokio::test]
async fn delete_uses_the_client_token_and_missing_tokens_are_reported() {
    let nodes = setup();
    let n = nodes.latest("n8n-nodes-base.gotify").unwrap();
    let (url, seen) = serve(vec![http_resp("200 OK", "")]).await;
    let params = json!({"resource": "message", "operation": "delete", "messageId": "12"});
    let out = n.execute(&ctx(params.clone(), both(&url))).await.unwrap();
    assert_eq!(out[0][0].json["success"], true);
    let req = seen.lock().unwrap()[0].clone();
    assert!(req.starts_with("DELETE /message/12 HTTP/1.1"), "{req}");
    assert_eq!(key(&req).as_deref(), Some("clitok"));

    // credential with only an app token cannot delete; nothing is sent
    let e = n
        .execute(&ctx(
            params,
            json!({"url": "http://127.0.0.1:1", "appApiToken": "APPTOK"}),
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("Client API Token") && !e.contains("APPTOK"),
        "{e}"
    );
}

#[tokio::test]
async fn errors_do_not_leak_tokens() {
    let nodes = setup();
    let (url, _s) = serve(vec![http_resp(
        "401 Unauthorized",
        r#"{"error":"Unauthorized","errorCode":401,"errorDescription":"you need to provide a valid access token or user credentials to access this api"}"#,
    )])
    .await;
    let params = json!({"resource": "message", "operation": "create", "message": "x"});
    let e = nodes
        .latest("n8n-nodes-base.gotify")
        .unwrap()
        .execute(&ctx(params, both(&url)))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("401") && e.contains("valid access token") && e.contains("credentials"),
        "{e}"
    );
    assert!(!e.contains("APPTOK") && !e.contains("CLITOK"));
}
