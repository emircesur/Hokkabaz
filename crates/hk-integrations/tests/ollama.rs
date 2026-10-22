//! Ollama node against a local fake server.
#![cfg(feature = "int-ollama")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::{CredentialTypes, register_integrations};
use serde_json::{Value, json};

use common::{Creds, http_resp, serve};

async fn run(base: &str, params: Value) -> Result<Vec<Item>, hk_flow::FlowError> {
    let mut nodes = NodeRegistry::new();
    register_integrations(&mut nodes, &CredentialTypes::new()).unwrap();
    let node: Node = serde_json::from_value(json!({
        "name": "Ollama", "type": "hokkabaz.ollama", "typeVersion": 1, "position": [0, 0],
        "parameters": params,
        "credentials": {"ollamaApi": {"id": "c1", "name": "o"}}
    }))
    .unwrap();
    let mut c = NodeContext::new(node, vec![vec![Item::from_json(json!({}))]], 0);
    c.hooks.credentials = Some(Arc::new(Creds(json!({"baseUrl": base}))));
    nodes.latest("hokkabaz.ollama").unwrap().execute(&c).await.map(|mut o| o.remove(0))
}

fn body_of(req: &str) -> Value {
    serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

#[tokio::test]
async fn generate_and_chat_bodies() {
    let (url, seen) = serve(vec![
        http_resp("200 OK", r#"{"model":"m","response":"hi","done":true,"context":[1,2,3]}"#),
        http_resp("200 OK", r#"{"model":"m","message":{"role":"assistant","content":"yo"},"done":true}"#),
    ])
    .await;
    let out = run(&url, json!({"operation": "generate", "model": "m", "prompt": "p", "system": "s", "options": {"temperature": 0.2, "num_predict": 64, "json": true}})).await.unwrap();
    assert_eq!(out[0].json["response"], "hi");
    assert!(out[0].json.get("context").is_none());
    let out = run(&url, json!({"operation": "chat", "model": "m", "prompt": "now", "system": "sys", "messages": [{"role": "user", "content": "before"}, {"role": "assistant", "content": "ok"}]})).await.unwrap();
    assert_eq!(out[0].json["message"]["content"], "yo");

    let reqs = seen.lock().unwrap().clone();
    assert!(reqs[0].starts_with("POST /api/generate HTTP/1.1"), "{}", reqs[0]);
    assert_eq!(body_of(&reqs[0]), json!({"model": "m", "prompt": "p", "system": "s", "stream": false, "format": "json", "options": {"temperature": 0.2, "num_predict": 64}}));
    assert!(reqs[1].starts_with("POST /api/chat HTTP/1.1"));
    let b = body_of(&reqs[1]);
    let roles: Vec<_> = b["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap().to_owned()).collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);
    assert_eq!(b["messages"][3]["content"], "now");
}

#[tokio::test]
async fn embeddings_and_list_models() {
    let (url, seen) = serve(vec![
        http_resp("200 OK", r#"{"model":"e","embeddings":[[0.1,0.2]]}"#),
        http_resp("200 OK", r#"{"models":[{"name":"a:latest","size":1},{"name":"b:7b","size":2}]}"#),
    ])
    .await;
    let out = run(&url, json!({"operation": "embeddings", "model": "e", "input": "text"})).await.unwrap();
    assert_eq!(out[0].json["embeddings"], json!([[0.1, 0.2]]));
    let out = run(&url, json!({"operation": "listModels"})).await.unwrap();
    assert_eq!(out.iter().map(|i| i.json["name"].as_str().unwrap()).collect::<Vec<_>>(), ["a:latest", "b:7b"]);
    let reqs = seen.lock().unwrap().clone();
    assert!(reqs[0].starts_with("POST /api/embed HTTP/1.1"));
    assert!(reqs[1].starts_with("GET /api/tags HTTP/1.1"));
}

#[tokio::test]
async fn pull_consumes_the_stream() {
    let ndjson = "{\"status\":\"pulling manifest\"}\n{\"status\":\"downloading\",\"completed\":1,\"total\":9}\n{\"status\":\"success\"}\n";
    let (url, seen) = serve(vec![http_resp("200 OK", ndjson), http_resp("200 OK", "{\"status\":\"pulling manifest\"}\n{\"error\":\"pull model manifest: file does not exist\"}\n")]).await;
    let out = run(&url, json!({"operation": "pullModel", "model": "llama3.2"})).await.unwrap();
    assert_eq!(Value::Object(out[0].json.clone()), json!({"success": true, "model": "llama3.2", "status": "success"}));
    assert_eq!(body_of(&seen.lock().unwrap()[0])["stream"], true);
    let err = run(&url, json!({"operation": "pullModel", "model": "nope"})).await.unwrap_err();
    assert!(err.to_string().contains("file does not exist"), "{err}");
}
