//! Delegating an unsupported node to an n8n instance, against a local fake of its API.
#![cfg(feature = "int-n8n")]

mod common;

use hk_flow::{FlowError, Item, Node, RemoteNodeRunner};
use hk_integrations::n8n_remote::{N8nClient, N8nRemote};
use serde_json::json;

use common::{http_resp, serve};

fn node() -> Node {
    serde_json::from_value(json!({
        "name": "Weird", "type": "n8n-nodes-base.somethingExotic", "typeVersion": 1, "position": [0, 0],
        "parameters": {"x": 1}
    }))
    .unwrap()
}

#[tokio::test]
async fn runs_through_a_temporary_webhook_workflow_and_cleans_up() {
    let (url, seen) = serve(vec![
        http_resp("200 OK", r#"{"id":"w1"}"#),                         // create
        http_resp("200 OK", r#"{"id":"w1","active":true}"#),           // activate
        http_resp("200 OK", r#"[{"a":1,"y":"ok"},{"a":2,"y":"ok"}]"#), // webhook
        http_resp("200 OK", r#"{"id":"w1"}"#),                         // deactivate
        http_resp("200 OK", r#"{"id":"w1"}"#),                         // delete
    ])
    .await;
    let remote = N8nRemote::new(N8nClient::new(&url, "KEY").unwrap());
    let out = remote.run_node(&node(), &[Item::from_json(json!({"a": 1})), Item::from_json(json!({"a": 2}))]).await.unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].len(), 2);
    assert_eq!(out[0][1].json["a"], 2);

    let seen = seen.lock().unwrap();
    assert!(seen[0].starts_with("POST /api/v1/workflows "), "{}", seen[0]);
    assert!(seen[0].to_lowercase().contains("x-n8n-api-key: key"));
    assert!(seen[0].contains("n8n-nodes-base.somethingExotic") && seen[0].contains("respondToWebhook"));
    assert!(seen[1].starts_with("POST /api/v1/workflows/w1/activate"));
    assert!(seen[2].starts_with("POST /webhook/hk-"), "{}", seen[2]);
    assert!(seen[2].contains(r#""items":[{"a":1},{"a":2}]"#));
    assert!(seen[3].starts_with("POST /api/v1/workflows/w1/deactivate"));
    assert!(seen[4].starts_with("DELETE /api/v1/workflows/w1"));
}

#[tokio::test]
async fn a_rejected_key_is_reported_without_leaking_it() {
    let (url, _) = serve(vec![http_resp("401 Unauthorized", r#"{"message":"unauthorized"}"#)]).await;
    let remote = N8nRemote::new(N8nClient::new(&url, "SECRETKEY").unwrap());
    let err = remote.run_node(&node(), &[]).await.unwrap_err();
    let FlowError::Node { message, .. } = err else { panic!("{err:?}") };
    assert!(message.contains("rejected") && message.contains("somethingExotic"), "{message}");
    assert!(!message.contains("SECRETKEY") && !message.contains("127.0.0.1"));
}
