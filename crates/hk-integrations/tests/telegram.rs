//! Telegram node and trigger against a local Bot API stand-in.
#![cfg(feature = "int-telegram")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::poll::{MemoryPollState, PollContext, PollStateStore, run_poll};
use hk_integrations::{CredentialTypes, PollRegistry, register_integrations_with_polling};
use serde_json::{Value, json};

use common::{Creds, http_resp, serve};

const TOKEN: &str = "123:SECRET";

fn setup() -> (NodeRegistry, CredentialTypes, PollRegistry) {
    let mut nodes = NodeRegistry::new();
    let creds = CredentialTypes::new();
    let mut polls = PollRegistry::new();
    register_integrations_with_polling(&mut nodes, &creds, &mut polls).unwrap();
    (nodes, creds, polls)
}

fn node(ty: &str, params: Value) -> Node {
    serde_json::from_value(json!({
        "name": "Telegram", "type": ty, "typeVersion": 1.2, "position": [0, 0],
        "parameters": params,
        "credentials": {"telegramApi": {"id": "c1", "name": "tg"}}
    }))
    .unwrap()
}

fn creds_for(url: &str) -> Arc<Creds> {
    Arc::new(Creds(json!({"accessToken": TOKEN, "baseUrl": url})))
}

fn ctx(params: Value, url: &str, item: Item) -> NodeContext {
    let mut c = NodeContext::new(node("n8n-nodes-base.telegram", params), vec![vec![item]], 0);
    c.hooks.credentials = Some(creds_for(url));
    c
}

fn body_of(req: &str) -> Value {
    serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

#[test]
fn registers_node_trigger_and_credential() {
    let (nodes, creds, polls) = setup();
    assert!(nodes.latest("n8n-nodes-base.telegram").is_some());
    // n8n resolves by exact typeVersion: 1.2 (node() default) must be served
    assert!(nodes.is_supported(&node("n8n-nodes-base.telegram", json!({}))));
    assert!(polls.get("n8n-nodes-base.telegramTrigger").is_some());
    let s = creds.schema("telegramApi").unwrap();
    assert_eq!(
        s["properties"]["baseUrl"]["default"],
        "https://api.telegram.org"
    );
    assert_eq!(s["required"], json!(["accessToken"]));
}

#[tokio::test]
async fn send_message_routes_to_bot_token_path_with_json_body() {
    let (nodes, ..) = setup();
    let (url, seen) = serve(vec![http_resp(
        "200 OK",
        r#"{"ok":true,"result":{"message_id":9}}"#,
    )])
    .await;
    let params = json!({
        "resource": "message", "operation": "sendMessage", "chatId": "-100123", "text": "hello",
        "additionalFields": {"parse_mode": "HTML", "reply_to_message_id": "7", "appendAttribution": false},
        "replyMarkup": "inlineKeyboard",
        "inlineKeyboard": {"rows": [{"row": {"buttons": [
            {"text": "Go", "additionalFields": {"callback_data": "go"}}]}}]}
    });
    let c = ctx(params, &url, Item::from_json(json!({})));
    let out = nodes
        .latest("n8n-nodes-base.telegram")
        .unwrap()
        .execute(&c)
        .await
        .unwrap();
    assert_eq!(out[0][0].json["result"]["message_id"], 9);

    let req = seen.lock().unwrap()[0].clone();
    assert!(
        req.starts_with(&format!("POST /bot{TOKEN}/sendMessage HTTP/1.1")),
        "{req}"
    );
    assert_eq!(
        body_of(&req),
        json!({
            "chat_id": "-100123", "text": "hello", "parse_mode": "HTML", "reply_to_message_id": 7,
            "reply_markup": {"inline_keyboard": [[{"text": "Go", "callback_data": "go"}]]}
        })
    );
}

#[tokio::test]
async fn other_message_operations_hit_their_methods() {
    let (nodes, ..) = setup();
    let ok = http_resp("200 OK", r#"{"ok":true,"result":true}"#);
    let (url, seen) = serve(vec![
        ok.clone(),
        ok.clone(),
        ok,
        http_resp(
            "200 OK",
            r#"{"ok":true,"result":{"id":5,"type":"private"}}"#,
        ),
    ])
    .await;
    let n = nodes.latest("n8n-nodes-base.telegram").unwrap();
    for params in [
        json!({"resource": "message", "operation": "editMessageText", "chatId": "1", "messageId": "42", "text": "new"}),
        json!({"resource": "message", "operation": "deleteMessage", "chatId": "1", "messageId": "42"}),
        json!({"resource": "callback", "operation": "answerQuery", "queryId": "q9", "additionalFields": {"text": "done", "show_alert": true, "cache_time": 5}}),
        json!({"resource": "chat", "operation": "get", "chatId": "5"}),
    ] {
        n.execute(&ctx(params, &url, Item::from_json(json!({}))))
            .await
            .unwrap();
    }
    let reqs = seen.lock().unwrap().clone();
    let line = |i: usize| reqs[i].lines().next().unwrap().to_owned();
    assert_eq!(
        line(0),
        format!("POST /bot{TOKEN}/editMessageText HTTP/1.1")
    );
    assert_eq!(
        body_of(&reqs[0]),
        json!({"chat_id": "1", "message_id": 42, "text": "new"})
    );
    assert_eq!(line(1), format!("POST /bot{TOKEN}/deleteMessage HTTP/1.1"));
    assert_eq!(body_of(&reqs[1]), json!({"chat_id": "1", "message_id": 42}));
    assert_eq!(
        line(2),
        format!("POST /bot{TOKEN}/answerCallbackQuery HTTP/1.1")
    );
    assert_eq!(
        body_of(&reqs[2]),
        json!({"callback_query_id": "q9", "text": "done", "show_alert": true, "cache_time": 5})
    );
    assert_eq!(line(3), format!("POST /bot{TOKEN}/getChat HTTP/1.1"));
}

#[tokio::test]
async fn photo_from_binary_is_uploaded_as_multipart_and_url_is_sent_as_json() {
    let (nodes, ..) = setup();
    let (url, seen) = serve(vec![
        http_resp("200 OK", r#"{"ok":true,"result":{"message_id":1}}"#),
        http_resp("200 OK", r#"{"ok":true,"result":{"message_id":2}}"#),
    ])
    .await;
    let n = nodes.latest("n8n-nodes-base.telegram").unwrap();

    let item: Item = serde_json::from_value(json!({
        "json": {}, "binary": {"data": {"data": "UE5HREFUQQ==", "mimeType": "image/png", "fileName": "a.png"}}
    }))
    .unwrap();
    let params = json!({
        "resource": "message", "operation": "sendPhoto", "chatId": "1",
        "binaryData": true, "binaryPropertyName": "data", "additionalFields": {"caption": "cap"}
    });
    n.execute(&ctx(params, &url, item)).await.unwrap();
    let params = json!({
        "resource": "message", "operation": "sendDocument", "chatId": "1", "file": "https://x.test/f.pdf"
    });
    n.execute(&ctx(params, &url, Item::from_json(json!({}))))
        .await
        .unwrap();

    let reqs = seen.lock().unwrap().clone();
    assert!(reqs[0].starts_with(&format!("POST /bot{TOKEN}/sendPhoto HTTP/1.1")));
    assert!(
        reqs[0]
            .to_lowercase()
            .contains("content-type: multipart/form-data; boundary=")
    );
    assert!(reqs[0].contains("name=\"chat_id\"\r\n\r\n1"));
    assert!(reqs[0].contains("name=\"caption\"\r\n\r\ncap"));
    assert!(reqs[0].contains("name=\"photo\"; filename=\"a.png\""));
    assert!(reqs[0].contains("Content-Type: image/png\r\n\r\nPNGDATA"));
    assert_eq!(
        body_of(&reqs[1]),
        json!({"chat_id": "1", "document": "https://x.test/f.pdf"})
    );
}

#[tokio::test]
async fn file_get_can_download_into_binary() {
    let (nodes, ..) = setup();
    let file_resp = "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 4\r\nConnection: close\r\n\r\nJPEG".to_owned();
    let (url, seen) = serve(vec![
        http_resp(
            "200 OK",
            r#"{"ok":true,"result":{"file_id":"f1","file_path":"photos/p.jpg"}}"#,
        ),
        file_resp,
    ])
    .await;
    let params = json!({"resource": "file", "operation": "get", "fileId": "f1", "download": true});
    let out = nodes
        .latest("n8n-nodes-base.telegram")
        .unwrap()
        .execute(&ctx(params, &url, Item::from_json(json!({}))))
        .await
        .unwrap();
    let bin = out[0][0].binary.as_ref().unwrap();
    assert_eq!(bin["data"].file_name.as_deref(), Some("p.jpg"));
    assert_eq!(bin["data"].mime_type, "image/jpeg");
    assert_eq!(bin["data"].data, "SlBFRw==");
    let reqs = seen.lock().unwrap().clone();
    assert!(
        reqs[1].starts_with(&format!("GET /file/bot{TOKEN}/photos/p.jpg HTTP/1.1")),
        "{}",
        reqs[1]
    );
}

#[tokio::test]
async fn api_errors_are_mapped_and_never_leak_the_token() {
    let (nodes, ..) = setup();
    let n = nodes.latest("n8n-nodes-base.telegram").unwrap();
    let params =
        json!({"resource": "message", "operation": "sendMessage", "chatId": "1", "text": "x"});
    let (url, _s) = serve(vec![http_resp(
        "400 Bad Request",
        r#"{"ok":false,"error_code":400,"description":"Bad Request: chat not found"}"#,
    )])
    .await;
    let e = n
        .execute(&ctx(params.clone(), &url, Item::from_json(json!({}))))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("400") && e.contains("chat not found"), "{e}");
    assert!(!e.contains("SECRET"), "{e}");

    // connection failure: reqwest's message would carry the URL (and so the token)
    let e = n
        .execute(&ctx(
            params.clone(),
            "http://127.0.0.1:1",
            Item::from_json(json!({})),
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(!e.contains("SECRET"), "{e}");

    let (url, _s) = serve(vec![http_resp(
        "401 Unauthorized",
        r#"{"ok":false,"description":"Unauthorized"}"#,
    )])
    .await;
    let mut c = ctx(params, &url, Item::from_json(json!({})));
    c.node.continue_on_fail = Some(true);
    let out = n.execute(&c).await.unwrap();
    assert!(out[0][0].json["error"].as_str().unwrap().contains("401"));
    assert!(out[0][0].error.is_some());
}

fn trigger_node(params: Value) -> Node {
    node("n8n-nodes-base.telegramTrigger", params)
}

#[tokio::test]
async fn trigger_long_polls_get_updates_with_filters_and_offset_cursor() {
    let (_n, _c, polls) = setup();
    let trig = polls.get("n8n-nodes-base.telegramTrigger").unwrap().clone();
    let updates = json!({"ok": true, "result": [
        {"update_id": 10, "message": {"chat": {"id": 100}, "from": {"id": 1}, "text": "a"}},
        {"update_id": 11, "message": {"chat": {"id": 200}, "from": {"id": 1}, "text": "other chat"}},
        {"update_id": 12, "edited_message": {"chat": {"id": 100}, "from": {"id": 1}, "text": "edit"}},
        {"update_id": 13, "message": {"chat": {"id": 100}, "from": {"id": 2}, "text": "b"}}
    ]});
    let (url, seen) = serve(vec![
        http_resp("200 OK", &updates.to_string()),
        http_resp("200 OK", r#"{"ok":true,"result":[]}"#),
    ])
    .await;

    let n = trigger_node(json!({
        "updates": ["message"],
        "additionalFields": {"chatIds": "100", "timeout": 3}
    }));
    let cx = PollContext::for_node(&n, Some("wf"), Some(creds_for(&url)));
    let store = MemoryPollState::default();

    let o = run_poll(trig.as_ref(), &store, &cx, "wf").await.unwrap();
    // pending updates are events: the first poll emits
    let texts: Vec<_> = o
        .items
        .iter()
        .map(|i| i.json["message"]["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(texts, ["a", "b"]);
    assert_eq!(o.items[0].json["update_id"], 10);

    // filtered-out updates are still confirmed: next offset is last id + 1
    let o = run_poll(trig.as_ref(), &store, &cx, "wf").await.unwrap();
    assert!(o.items.is_empty());
    assert_eq!(
        store.load("wf", "Telegram").await.unwrap().unwrap()["cursor"],
        14
    );

    let reqs = seen.lock().unwrap().clone();
    assert!(reqs[0].starts_with(&format!("POST /bot{TOKEN}/getUpdates HTTP/1.1")));
    assert_eq!(
        body_of(&reqs[0]),
        json!({"timeout": 3, "allowed_updates": ["message"]})
    );
    assert_eq!(
        body_of(&reqs[1]),
        json!({"timeout": 3, "offset": 14, "allowed_updates": ["message"]})
    );
}

#[tokio::test]
async fn trigger_star_asks_for_all_update_types_and_reports_conflicts() {
    let (_n, _c, polls) = setup();
    let trig = polls.get("n8n-nodes-base.telegramTrigger").unwrap().clone();
    let (url, seen) = serve(vec![
        http_resp("200 OK", r#"{"ok":true,"result":[{"update_id":1,"callback_query":{"id":"q","from":{"id":3}}}]}"#),
        http_resp(
            "409 Conflict",
            r#"{"ok":false,"error_code":409,"description":"Conflict: can't use getUpdates method while webhook is active"}"#,
        ),
    ])
    .await;
    let n = trigger_node(json!({"updates": ["*"]}));
    let cx = PollContext::for_node(&n, Some("wf"), Some(creds_for(&url)));
    let store = MemoryPollState::default();
    let o = run_poll(trig.as_ref(), &store, &cx, "wf").await.unwrap();
    assert_eq!(o.items.len(), 1);
    assert_eq!(body_of(&seen.lock().unwrap()[0]), json!({"timeout": 0}));

    let e = run_poll(trig.as_ref(), &store, &cx, "wf")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("409") && e.contains("webhook is active") && !e.contains("SECRET"),
        "{e}"
    );
}
