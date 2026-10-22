//! Gitea node against a local server: `/api/v1` routing, token auth, create-vs-update file.
#![cfg(feature = "int-gitea")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::{CredentialTypes, register_integrations};
use serde_json::{Value, json};

use common::{Creds, http_resp, serve};

fn ctx(url: &str, params: Value) -> NodeContext {
    let node: Node = serde_json::from_value(json!({
        "name": "Gitea", "type": "hokkabaz.gitea", "typeVersion": 1, "position": [0, 0],
        "parameters": params,
        "credentials": {"giteaApi": {"id": "c1", "name": "g"}}
    }))
    .unwrap();
    let mut c = NodeContext::new(node, vec![vec![Item::from_json(json!({}))]], 0);
    // a server URL that already ends in /api/v1 is tolerated
    c.hooks.credentials = Some(Arc::new(Creds(
        json!({"server": format!("{url}/api/v1/"), "accessToken": "gt_SECRET"}),
    )));
    c
}

fn body_of(req: &str) -> Value {
    serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

#[tokio::test]
async fn issue_create_file_create_and_edit() {
    let mut nodes = NodeRegistry::new();
    let creds = CredentialTypes::new();
    register_integrations(&mut nodes, &creds).unwrap();
    assert!(creds.get("giteaApi").is_some());
    let gitea = nodes.latest("hokkabaz.gitea").unwrap();

    let (url, seen) = serve(vec![http_resp("201 Created", r#"{"number":3}"#)]).await;
    let params = json!({
        "resource": "issue", "operation": "create", "owner": "o", "repository": "r",
        "title": "Bug", "labels": "4, 5", "assignees": "bob"
    });
    let out = gitea.execute(&ctx(&url, params)).await.unwrap();
    assert_eq!(out[0][0].json["number"], 3);
    let req = seen.lock().unwrap()[0].clone();
    assert!(
        req.starts_with("POST /api/v1/repos/o/r/issues HTTP/1.1"),
        "{req}"
    );
    assert!(
        req.to_lowercase()
            .contains("authorization: token gt_secret")
    );
    assert_eq!(
        body_of(&req),
        json!({"title": "Bug", "labels": [4, 5], "assignees": ["bob"]})
    );

    // create = POST without sha
    let (url, seen) = serve(vec![http_resp("201 Created", r#"{"content":{}}"#)]).await;
    let params = json!({
        "resource": "file", "operation": "create", "owner": "o", "repository": "r",
        "filePath": "a/b.md", "fileContent": "hi", "commitMessage": "add"
    });
    gitea.execute(&ctx(&url, params)).await.unwrap();
    let req = seen.lock().unwrap()[0].clone();
    assert!(
        req.starts_with("POST /api/v1/repos/o/r/contents/a/b.md HTTP/1.1"),
        "{req}"
    );
    assert_eq!(body_of(&req), json!({"message": "add", "content": "aGk="}));

    // edit = GET sha, then PUT with it
    let (url, seen) = serve(vec![
        http_resp("200 OK", r#"{"sha":"s1"}"#),
        http_resp("200 OK", r#"{"content":{}}"#),
    ])
    .await;
    let params = json!({
        "resource": "file", "operation": "edit", "owner": "o", "repository": "r",
        "filePath": "a/b.md", "fileContent": "yo", "commitMessage": "upd"
    });
    gitea.execute(&ctx(&url, params)).await.unwrap();
    let seen = seen.lock().unwrap();
    assert!(
        seen[0].starts_with("GET /api/v1/repos/o/r/contents/a/b.md HTTP/1.1"),
        "{}",
        seen[0]
    );
    assert!(seen[1].starts_with("PUT /api/v1/repos/o/r/contents/a/b.md HTTP/1.1"));
    assert_eq!(body_of(&seen[1])["sha"], "s1");
}
