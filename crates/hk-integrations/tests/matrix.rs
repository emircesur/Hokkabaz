//! Matrix node against a local homeserver stand-in.
#![cfg(feature = "int-matrix")]

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

fn ctx(params: Value, url: &str) -> NodeContext {
    let node: Node = serde_json::from_value(json!({
        "name": "Matrix", "type": "n8n-nodes-base.matrix", "typeVersion": 1, "position": [0, 0],
        "parameters": params,
        "credentials": {"matrixApi": {"id": "c1", "name": "mx"}}
    }))
    .unwrap();
    let mut c = NodeContext::new(node, vec![vec![Item::from_json(json!({}))]], 0);
    c.hooks.credentials = Some(Arc::new(Creds(json!({"homeserverUrl": url, "accessToken": "mx_SECRET"}))));
    c
}

fn body_of(req: &str) -> Value {
    serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

fn has_bearer(req: &str) -> bool {
    req.to_lowercase().contains("authorization: bearer mx_secret")
}

#[test]
fn registers_node_and_credential() {
    let nodes = setup();
    assert!(nodes.latest("n8n-nodes-base.matrix").is_some());
    let creds = CredentialTypes::new();
    register_integrations(&mut NodeRegistry::new(), &creds).unwrap();
    let s = creds.schema("matrixApi").unwrap();
    assert_eq!(s["properties"]["homeserverUrl"]["default"], "https://matrix-client.matrix.org");
}

#[tokio::test]
async fn sends_a_message_with_bearer_auth_and_txn_id() {
    let nodes = setup();
    let (url, seen) = serve(vec![
        http_resp("200 OK", r#"{"event_id":"$e1"}"#),
        http_resp("200 OK", r#"{"event_id":"$e2"}"#),
    ])
    .await;
    let n = nodes.latest("n8n-nodes-base.matrix").unwrap();
    let out = n
        .execute(&ctx(
            json!({"resource": "message", "operation": "create", "roomId": "!room:hs.example", "text": "hi", "messageType": "m.notice"}),
            &url,
        ))
        .await
        .unwrap();
    assert_eq!(out[0][0].json["event_id"], "$e1");
    n.execute(&ctx(
        json!({"resource": "message", "operation": "create", "roomId": "!room:hs.example", "text": "<b>hi</b>",
               "messageFormat": "org.matrix.custom.html", "fallbackText": "hi"}),
        &url,
    ))
    .await
    .unwrap();

    let reqs = seen.lock().unwrap().clone();
    let line = reqs[0].lines().next().unwrap();
    assert!(
        line.starts_with("PUT /_matrix/client/v3/rooms/!room:hs.example/send/m.room.message/hk"),
        "{line}"
    );
    assert!(has_bearer(&reqs[0]));
    assert_eq!(body_of(&reqs[0]), json!({"msgtype": "m.notice", "body": "hi"}));
    assert_eq!(
        body_of(&reqs[1]),
        json!({"msgtype": "m.text", "body": "hi", "format": "org.matrix.custom.html", "formatted_body": "<b>hi</b>"})
    );
    // each send needs its own transaction id
    assert_ne!(reqs[0].lines().next(), reqs[1].lines().next());
}

#[tokio::test]
async fn get_all_pages_backwards_until_limit_or_end() {
    let nodes = setup();
    let n = nodes.latest("n8n-nodes-base.matrix").unwrap();
    let base = json!({"resource": "message", "operation": "getAll", "roomId": "!r:hs"});

    let (url, seen) = serve(vec![
        http_resp("200 OK", r#"{"chunk":[{"event_id":"$3"},{"event_id":"$2"}],"end":"t2"}"#),
        http_resp("200 OK", r#"{"chunk":[{"event_id":"$1"}],"end":"t1"}"#),
        http_resp("200 OK", r#"{"chunk":[]}"#),
    ])
    .await;
    let mut p = base.clone();
    p["returnAll"] = json!(true);
    let out = n.execute(&ctx(p, &url)).await.unwrap();
    let ids: Vec<_> = out[0].iter().map(|i| i.json["event_id"].as_str().unwrap().to_owned()).collect();
    assert_eq!(ids, ["$3", "$2", "$1"]);
    let reqs = seen.lock().unwrap().clone();
    assert!(reqs[0].starts_with("GET /_matrix/client/v3/rooms/!r:hs/messages?dir=b&limit=100 HTTP/1.1"), "{}", reqs[0]);
    assert!(reqs[1].contains("from=t2"), "{}", reqs[1]);
    assert!(has_bearer(&reqs[1]));

    let (url, seen) = serve(vec![http_resp(
        "200 OK",
        r#"{"chunk":[{"event_id":"$3"},{"event_id":"$2"}],"end":"t2"}"#,
    )])
    .await;
    let mut p = base;
    p["limit"] = json!(2);
    let out = n.execute(&ctx(p, &url)).await.unwrap();
    assert_eq!(out[0].len(), 2);
    assert!(seen.lock().unwrap()[0].contains("limit=2"));
    assert_eq!(seen.lock().unwrap().len(), 1, "limit reached: no second page");
}

#[tokio::test]
async fn room_and_event_operations_route_correctly() {
    let nodes = setup();
    let n = nodes.latest("n8n-nodes-base.matrix").unwrap();
    let ok = |b: &str| http_resp("200 OK", b);
    let (url, seen) = serve(vec![
        ok(r#"{"room_id":"!new:hs"}"#),
        ok(r#"{"room_id":"!r:hs"}"#),
        ok("{}"),
        ok("{}"),
        ok("{}"),
        ok(r#"{"event_id":"$x","type":"m.room.message"}"#),
    ])
    .await;
    let mut outs = Vec::new();
    for p in [
        json!({"resource": "room", "operation": "create", "roomName": "Ops", "preset": "private_chat", "roomAlias": "ops"}),
        json!({"resource": "room", "operation": "join", "roomIdOrAlias": "#ops:hs"}),
        json!({"resource": "room", "operation": "leave", "roomId": "!r:hs"}),
        json!({"resource": "room", "operation": "invite", "roomId": "!r:hs", "userId": "@bob:hs"}),
        json!({"resource": "room", "operation": "kick", "roomId": "!r:hs", "userId": "@eve:hs", "reason": "spam"}),
        json!({"resource": "event", "operation": "get", "roomId": "!r:hs", "eventId": "$x"}),
    ] {
        outs.push(n.execute(&ctx(p, &url)).await.unwrap());
    }
    let reqs = seen.lock().unwrap().clone();
    let line = |i: usize| reqs[i].lines().next().unwrap().to_owned();
    assert_eq!(line(0), "POST /_matrix/client/v3/createRoom HTTP/1.1");
    assert_eq!(body_of(&reqs[0]), json!({"name": "Ops", "preset": "private_chat", "room_alias_name": "ops"}));
    // `#` must not start a fragment
    assert_eq!(line(1), "POST /_matrix/client/v3/join/%23ops:hs HTTP/1.1");
    assert_eq!(line(2), "POST /_matrix/client/v3/rooms/!r:hs/leave HTTP/1.1");
    assert_eq!(line(3), "POST /_matrix/client/v3/rooms/!r:hs/invite HTTP/1.1");
    assert_eq!(body_of(&reqs[3]), json!({"user_id": "@bob:hs"}));
    assert_eq!(body_of(&reqs[4]), json!({"user_id": "@eve:hs", "reason": "spam"}));
    assert_eq!(line(5), "GET /_matrix/client/v3/rooms/!r:hs/event/$x HTTP/1.1");
    assert!(reqs.iter().all(|r| has_bearer(r)));
    // empty `{}` bodies become an explicit success item
    assert_eq!(outs[2][0][0].json["success"], true);
    assert_eq!(outs[5][0][0].json["type"], "m.room.message");
}

#[tokio::test]
async fn errors_are_mapped_and_never_leak_the_token() {
    let nodes = setup();
    let n = nodes.latest("n8n-nodes-base.matrix").unwrap();
    let (url, _s) = serve(vec![http_resp(
        "403 Forbidden",
        r#"{"errcode":"M_FORBIDDEN","error":"You are not in that room"}"#,
    )])
    .await;
    let e = n
        .execute(&ctx(json!({"resource": "room", "operation": "leave", "roomId": "!r:hs"}), &url))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("403") && e.contains("not in that room") && e.contains("credentials"), "{e}");
    assert!(!e.contains("mx_SECRET"));

    // required parameter missing: nothing is sent
    let e = n
        .execute(&ctx(json!({"resource": "room", "operation": "invite", "roomId": "!r:hs"}), "http://127.0.0.1:1"))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("User ID"), "{e}");
}
