//! SearXNG node and `web_search` tool against a local fake instance.
#![cfg(feature = "int-searxng")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::{CredentialTypes, register_integrations, searxng_web_search_tool};
use hk_tools::{Risk, ToolCtx};
use serde_json::{Value, json};

use common::{Creds, http_resp, serve};

const BODY: &str = r#"{"query":"rust","results":[
  {"title":"Rust","url":"https://rust-lang.org","content":"A language\nempowering everyone","engine":"ddg","engines":["ddg"],"score":1.5,"category":"general","parsed_url":["https","rust-lang.org"],"template":"default.html"},
  {"title":"Crates","url":"https://crates.io","content":"Registry","engine":"brave","category":"it"},
  {"title":"Third","url":"https://x.test"}]}"#;

#[tokio::test]
async fn node_maps_options_to_the_query_and_limits_results() {
    let mut nodes = NodeRegistry::new();
    register_integrations(&mut nodes, &CredentialTypes::new()).unwrap();
    let (url, seen) = serve(vec![http_resp("200 OK", BODY)]).await;
    let node: Node = serde_json::from_value(json!({
        "name": "S", "type": "hokkabaz.searxng", "typeVersion": 1, "position": [0, 0],
        "parameters": {"query": "rust lang", "limit": 2, "options": {"categories": "general,it", "language": "en", "timeRange": "month", "pageNo": 3}},
        "credentials": {"searxngApi": {"id": "c1", "name": "sx"}}
    }))
    .unwrap();
    let mut c = NodeContext::new(node, vec![vec![Item::from_json(json!({}))]], 0);
    c.hooks.credentials = Some(Arc::new(Creds(json!({"baseUrl": url, "apiKey": "TOK"}))));
    let out = nodes.latest("hokkabaz.searxng").unwrap().execute(&c).await.unwrap();
    assert_eq!(out[0].len(), 2);
    assert_eq!(out[0][0].json["title"], "Rust");
    assert!(out[0][0].json.get("parsed_url").is_none());
    let req = seen.lock().unwrap()[0].clone();
    let line = req.lines().next().unwrap();
    for part in ["GET /search?q=rust+lang", "format=json", "categories=general%2Cit", "language=en", "time_range=month", "pageno=3"] {
        assert!(line.contains(part), "{part} missing in {line}");
    }
    assert!(req.to_lowercase().contains("authorization: bearer tok"));
}

#[tokio::test]
async fn tool_formats_and_reports_errors() {
    let (url, _) = serve(vec![http_resp("200 OK", BODY), http_resp("403 Forbidden", "{}"), http_resp("200 OK", r#"{"results":[]}"#)]).await;
    let tool = searxng_web_search_tool(url);
    assert_eq!(tool.spec().name, "web_search");
    assert_eq!(tool.risk(&json!({})), Risk::Safe);

    let out = tool.execute(ToolCtx::default(), json!({"query": "rust", "max_results": 2})).await.unwrap();
    assert!(!out.is_error);
    assert!(out.content.starts_with("1. Rust\n   https://rust-lang.org\n   A language empowering everyone\n2. Crates"), "{}", out.content);
    assert!(!out.content.contains("3. Third"));

    let out = tool.execute(ToolCtx::default(), json!({"query": "rust"})).await.unwrap();
    assert!(out.is_error && out.content.contains("json"), "{}", out.content);

    let out = tool.execute(ToolCtx::default(), json!({"query": "nothing"})).await.unwrap();
    assert_eq!(out.content, "No results.");

    assert!(tool.execute(ToolCtx::default(), Value::Object(Default::default())).await.is_err());
}
