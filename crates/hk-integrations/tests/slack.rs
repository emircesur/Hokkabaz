//! Slack node against a local fake of the Web API: request shape, resource locators, paging, `ok:false`.
#![cfg(feature = "int-slack")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::{CredentialTypes, register_integrations};
use serde_json::{Value, json};

use common::{Creds, http_resp, serve};

fn ctx(params: Value, url: &str) -> NodeContext {
    let node: Node = serde_json::from_value(json!({
        "name": "Slack", "type": "n8n-nodes-base.slack", "typeVersion": 2.3, "position": [0, 0],
        "parameters": params,
        "credentials": {"slackApi": {"id": "c1", "name": "slack"}}
    }))
    .unwrap();
    let mut c = NodeContext::new(node, vec![vec![Item::from_json(json!({}))]], 0);
    c.hooks.credentials = Some(Arc::new(Creds(
        json!({"accessToken": "xoxb-SECRET", "apiUrl": url}),
    )));
    c
}

fn body_of(req: &str) -> Value {
    serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

#[tokio::test]
async fn slack_operations() {
    let mut nodes = NodeRegistry::new();
    register_integrations(&mut nodes, &CredentialTypes::new()).unwrap();
    let node = nodes.latest("n8n-nodes-base.slack").unwrap().clone();

    // post with a resource locator channel, thread reply and an ok:false failure after
    let (url, seen) = serve(vec![
        http_resp("200 OK", r#"{"ok":true,"channel":"C1","ts":"1.2","message":{"text":"hi"}}"#),
        http_resp("200 OK", r#"{"ok":false,"error":"not_in_channel"}"#),
        http_resp("200 OK", r#"{"ok":true,"channels":[{"id":"C1"},{"id":"C2"}],"response_metadata":{"next_cursor":"abc"}}"#),
        http_resp("200 OK", r#"{"ok":true,"channels":[{"id":"C3"}],"response_metadata":{"next_cursor":""}}"#),
    ])
    .await;
    let c = ctx(
        json!({"resource": "message", "operation": "post", "select": "channel",
        "channelId": {"__rl": true, "mode": "id", "value": "C1"}, "text": "hi",
        "otherOptions": {"thread_ts": "1.1", "unfurl_links": false}}),
        &url,
    );
    let out = node.execute(&c).await.unwrap();
    assert_eq!(out[0][0].json["ts"], "1.2");
    {
        let seen = seen.lock().unwrap();
        assert!(seen[0].starts_with("POST /chat.postMessage"), "{}", seen[0]);
        assert!(
            seen[0]
                .to_lowercase()
                .contains("authorization: bearer xoxb-secret")
        );
        assert_eq!(
            body_of(&seen[0]),
            json!({"channel": "C1", "text": "hi", "thread_ts": "1.1", "unfurl_links": false})
        );
    }

    let c = ctx(
        json!({"resource": "message", "operation": "post", "channelId": "C9", "text": "x"}),
        &url,
    );
    let err = node.execute(&c).await.unwrap_err().to_string();
    assert!(
        err.contains("not_in_channel") && err.contains("invite the bot"),
        "{err}"
    );
    assert!(!err.contains("xoxb-SECRET"));

    let c = ctx(
        json!({"resource": "channel", "operation": "getAll", "returnAll": true}),
        &url,
    );
    let out = node.execute(&c).await.unwrap();
    let ids: Vec<_> = out[0]
        .iter()
        .map(|i| i.json["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["C1", "C2", "C3"]);
    assert_eq!(body_of(&seen.lock().unwrap()[3])["cursor"], "abc");
}
