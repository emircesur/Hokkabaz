//! n8n node against a local server: api-key header, cursor pagination, path placeholders.
#![cfg(feature = "int-n8n")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::{CredentialTypes, register_integrations};
use serde_json::{Value, json};

use common::{Creds, http_resp, serve};

fn ctx(url: &str, params: Value) -> NodeContext {
    let node: Node = serde_json::from_value(json!({
        "name": "n8n", "type": "n8n-nodes-base.n8n", "typeVersion": 1, "position": [0, 0],
        "parameters": params,
        "credentials": {"n8nApi": {"id": "c1", "name": "n"}}
    }))
    .unwrap();
    let mut c = NodeContext::new(node, vec![vec![Item::from_json(json!({}))]], 0);
    c.hooks.credentials = Some(Arc::new(Creds(
        json!({"baseUrl": format!("{url}/api/v1"), "apiKey": "n8n_SECRET"}),
    )));
    c
}

#[tokio::test]
async fn workflow_list_pages_by_cursor_then_activate_and_execution_get() {
    let mut nodes = NodeRegistry::new();
    let creds = CredentialTypes::new();
    register_integrations(&mut nodes, &creds).unwrap();
    let n8n = nodes.latest("n8n-nodes-base.n8n").unwrap();

    let (url, seen) = serve(vec![
        http_resp(
            "200 OK",
            r#"{"data":[{"id":"1","name":"a"}],"nextCursor":"cur2"}"#,
        ),
        http_resp(
            "200 OK",
            r#"{"data":[{"id":"2","name":"b"}],"nextCursor":null}"#,
        ),
    ])
    .await;
    let params = json!({"resource": "workflow", "operation": "getAll", "returnAll": true, "filters": {"active": true}});
    let out = n8n.execute(&ctx(&url, params)).await.unwrap();
    let ids: Vec<_> = out[0]
        .iter()
        .map(|i| i.json["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["1", "2"]);
    {
        let seen = seen.lock().unwrap();
        let (l0, l1) = (
            seen[0].lines().next().unwrap(),
            seen[1].lines().next().unwrap(),
        );
        assert!(
            l0.starts_with("GET /api/v1/workflows?")
                && l0.contains("active=true")
                && l0.contains("limit=100"),
            "{l0}"
        );
        assert!(l1.contains("cursor=cur2"), "{l1}");
        assert!(seen[0].to_lowercase().contains("x-n8n-api-key: n8n_secret"));
    }

    let (url, seen) = serve(vec![http_resp("200 OK", r#"{"id":"9","active":true}"#)]).await;
    let params = json!({"resource": "workflow", "operation": "activate", "workflowId": {"__rl": true, "value": "9", "mode": "id"}});
    let out = n8n.execute(&ctx(&url, params)).await.unwrap();
    assert_eq!(out[0][0].json["active"], true);
    assert!(seen.lock().unwrap()[0].starts_with("POST /api/v1/workflows/9/activate HTTP/1.1"));

    let (url, seen) = serve(vec![http_resp("200 OK", r#"{"id":5,"status":"success"}"#)]).await;
    let params = json!({"resource": "execution", "operation": "get", "executionId": 5, "options": {"includeData": true}});
    let out = n8n.execute(&ctx(&url, params)).await.unwrap();
    assert_eq!(out[0][0].json["status"], "success");
    assert!(
        seen.lock().unwrap()[0].starts_with("GET /api/v1/executions/5?includeData=true HTTP/1.1")
    );
}
