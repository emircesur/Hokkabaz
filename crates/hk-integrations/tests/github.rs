//! GitHub node and polling trigger against a local server: routing, auth header, body
//! shaping, pagination, the file sha lookup and event filtering.
#![cfg(feature = "int-github")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::poll::PollContext;
use hk_integrations::{CredentialTypes, PollRegistry, register_integrations_with_polling};
use serde_json::{Value, json};

use common::{Creds, http_resp, serve};

struct Setup {
    nodes: NodeRegistry,
    creds: CredentialTypes,
    polls: PollRegistry,
}

fn setup() -> Setup {
    let mut nodes = NodeRegistry::new();
    let creds = CredentialTypes::new();
    let mut polls = PollRegistry::new();
    register_integrations_with_polling(&mut nodes, &creds, &mut polls).unwrap();
    Setup {
        nodes,
        creds,
        polls,
    }
}

fn node(ty: &str, params: Value) -> Node {
    serde_json::from_value(json!({
        "name": "GH", "type": ty, "typeVersion": 1, "position": [0, 0],
        "parameters": params,
        "credentials": {"githubApi": {"id": "c1", "name": "gh"}}
    }))
    .unwrap()
}

fn creds_for(url: &str) -> Arc<Creds> {
    Arc::new(Creds(json!({"server": url, "accessToken": "ghp_SECRET"})))
}

fn ctx(url: &str, params: Value) -> NodeContext {
    let mut c = NodeContext::new(
        node("n8n-nodes-base.github", params),
        vec![vec![Item::from_json(json!({}))]],
        0,
    );
    c.hooks.credentials = Some(creds_for(url));
    c
}

fn request_line(req: &str) -> &str {
    req.lines().next().unwrap()
}

fn body_of(req: &str) -> Value {
    serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

#[test]
fn registers_node_trigger_and_credential() {
    let s = setup();
    assert!(s.nodes.latest("n8n-nodes-base.github").is_some());
    assert!(s.polls.get("n8n-nodes-base.githubTrigger").is_some());
    let schema = s.creds.schema("githubApi").unwrap();
    assert_eq!(
        schema["properties"]["server"]["default"],
        "https://api.github.com"
    );
    assert_eq!(schema["required"], json!(["accessToken"]));
}

#[tokio::test]
async fn issue_create_routes_with_auth_and_body() {
    let s = setup();
    let (url, seen) = serve(vec![http_resp(
        "201 Created",
        r#"{"number":7,"title":"T"}"#,
    )])
    .await;
    let params = json!({
        "resource": "issue", "operation": "create", "owner": "acme", "repository": "wid get",
        "title": "T", "body": "B", "labels": "bug, ui", "assignees": [{"assignee": "alice"}]
    });
    let out = s
        .nodes
        .latest("n8n-nodes-base.github")
        .unwrap()
        .execute(&ctx(&url, params))
        .await
        .unwrap();
    assert_eq!(out[0][0].json["number"], 7);

    let req = seen.lock().unwrap()[0].clone();
    assert_eq!(
        request_line(&req),
        "POST /repos/acme/wid%20get/issues HTTP/1.1"
    );
    let low = req.to_lowercase();
    assert!(low.contains("authorization: token ghp_secret"), "{req}");
    assert!(low.contains("user-agent: hokkabaz") && low.contains("application/vnd.github+json"));
    assert_eq!(
        body_of(&req),
        json!({"title": "T", "body": "B", "labels": ["bug", "ui"], "assignees": ["alice"]})
    );
}

#[tokio::test]
async fn errors_map_and_do_not_leak_the_token() {
    let s = setup();
    let (url, _seen) = serve(vec![http_resp(
        "404 Not Found",
        r#"{"message":"Not Found"}"#,
    )])
    .await;
    let params =
        json!({"resource": "repository", "operation": "get", "owner": "a", "repository": "b"});
    let e = s
        .nodes
        .latest("n8n-nodes-base.github")
        .unwrap()
        .execute(&ctx(&url, params))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("404") && e.contains("Not Found"), "{e}");
    assert!(!e.contains("ghp_SECRET"));
}

fn issues(range: std::ops::Range<u32>) -> String {
    let v: Vec<Value> = range.map(|n| json!({"number": n})).collect();
    http_resp("200 OK", &Value::Array(v).to_string())
}

#[tokio::test]
async fn get_all_pages_until_short_page_and_honours_limit() {
    let s = setup();
    let gh = s.nodes.latest("n8n-nodes-base.github").unwrap();

    // 100 = a full page, so the node asks for page 2 and stops on the short one
    let (url, seen) = serve(vec![issues(0..100), issues(100..101)]).await;
    let params = json!({
        "resource": "repository", "operation": "getIssues", "owner": "a", "repository": "b",
        "returnAll": true, "getRepositoryIssuesFilters": {"state": "closed", "labels": "bug"}
    });
    let out = gh.execute(&ctx(&url, params)).await.unwrap();
    assert_eq!(out[0].len(), 101);
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let (l0, l1) = (request_line(&seen[0]), request_line(&seen[1]));
        assert!(l0.starts_with("GET /repos/a/b/issues?"), "{l0}");
        for q in ["state=closed", "labels=bug", "per_page=100", "page=1"] {
            assert!(l0.contains(q), "{q} in {l0}");
        }
        assert!(l1.contains("page=2") && l1.contains("state=closed"), "{l1}");
    }

    let (url, seen) = serve(vec![issues(0..2)]).await;
    let params = json!({"resource": "release", "operation": "getAll", "owner": "a", "repository": "b", "limit": 2});
    let out = gh.execute(&ctx(&url, params)).await.unwrap();
    assert_eq!(out[0].len(), 2);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(request_line(&seen[0]).starts_with("GET /repos/a/b/releases?"));
    assert!(request_line(&seen[0]).contains("per_page=2"));
}

#[tokio::test]
async fn file_edit_reads_the_sha_first() {
    let s = setup();
    let (url, seen) = serve(vec![
        http_resp("200 OK", r#"{"sha":"abc123","type":"file"}"#),
        http_resp(
            "200 OK",
            r#"{"content":{"path":"dir/a b.txt"},"commit":{"sha":"c1"}}"#,
        ),
    ])
    .await;
    let params = json!({
        "resource": "file", "operation": "edit", "owner": "a", "repository": "b",
        "filePath": "dir/a b.txt", "fileContent": "hi", "commitMessage": "update",
        "additionalParameters": {"branch": {"branch": "dev"}, "authorName": "Al", "authorEmail": "al@x.io"}
    });
    let out = s
        .nodes
        .latest("n8n-nodes-base.github")
        .unwrap()
        .execute(&ctx(&url, params))
        .await
        .unwrap();
    assert_eq!(out[0][0].json["commit"]["sha"], "c1");

    let seen = seen.lock().unwrap();
    assert_eq!(
        request_line(&seen[0]),
        "GET /repos/a/b/contents/dir/a%20b.txt?ref=dev HTTP/1.1"
    );
    assert_eq!(
        request_line(&seen[1]),
        "PUT /repos/a/b/contents/dir/a%20b.txt HTTP/1.1"
    );
    assert_eq!(
        body_of(&seen[1]),
        json!({
            "message": "update", "content": "aGk=", "sha": "abc123", "branch": "dev",
            "author": {"name": "Al", "email": "al@x.io"}
        })
    );
}

#[tokio::test]
async fn trigger_emits_only_selected_events_oldest_first() {
    let s = setup();
    let trigger = s.polls.get("n8n-nodes-base.githubTrigger").unwrap().clone();
    let feed = json!([
        {"id": "3", "type": "IssuesEvent", "payload": {"action": "opened"}},
        {"id": "2", "type": "PushEvent", "payload": {}},
        {"id": "1", "type": "IssuesEvent", "payload": {"action": "closed"}},
    ]);
    let (url, seen) = serve(vec![http_resp("200 OK", &feed.to_string())]).await;
    let n = node(
        "n8n-nodes-base.githubTrigger",
        json!({"owner": "acme", "repository": "widget", "events": ["issues", "release"]}),
    );
    let cx = PollContext::for_node(&n, Some("wf"), Some(creds_for(&url)));
    let batch = trigger.poll(&cx, None).await.unwrap();
    let ids: Vec<_> = batch.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["1", "3"]);
    assert_eq!(batch.items[1].json["payload"]["action"], "opened");

    let req = seen.lock().unwrap()[0].clone();
    assert!(
        request_line(&req).starts_with("GET /repos/acme/widget/events?per_page=100"),
        "{req}"
    );
    assert!(
        req.to_lowercase()
            .contains("authorization: token ghp_secret")
    );

    // `*` takes everything
    let (url, _s) = serve(vec![http_resp("200 OK", &feed.to_string())]).await;
    let n = node(
        "n8n-nodes-base.githubTrigger",
        json!({"owner": "a", "repository": "b", "events": "*"}),
    );
    let cx = PollContext::for_node(&n, Some("wf"), Some(creds_for(&url)));
    assert_eq!(trigger.poll(&cx, None).await.unwrap().items.len(), 3);
}
