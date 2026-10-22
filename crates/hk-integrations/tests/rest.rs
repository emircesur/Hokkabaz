//! RestNode framework behaviour (pagination, simplify, limit, query/header routing,
//! credential application) with a throwaway node, independent of any integration.

mod common;

use std::sync::Arc;

use hk_flow::{FlowNode, Item, Node, NodeContext};
use hk_integrations::describe::{
    Choice, NodeBuilder, Pagination, Prop, RouteSend, Routing, return_all_and_limit, simplify_prop,
};
use hk_integrations::rest::{RestNode, RestSpec};
use hk_integrations::{Authenticate, CredentialType, CredentialTypes};
use serde_json::{Value, json};

use common::{Creds, http_resp, serve};

fn node(pagination: Pagination) -> RestNode {
    let creds = CredentialTypes::new();
    creds.register_if_absent(
        CredentialType::new("demoApi", "Demo API")
            .property(Prop::string("baseUrl", "Base URL").required())
            .property(Prop::string("key", "Key").password().required())
            .authenticate(Authenticate::header("X-Key", "={{$credentials.key}}")),
    );
    let desc = NodeBuilder::new("hokkabaz.demoRest", "Demo REST")
        .credential("demoApi", true)
        .property(Prop::options(
            "operation",
            "Operation",
            vec![
                Choice::new("List", "list").routing(
                    Routing::request("GET", "=/things/{{ $json.kind }}")
                        .qs("fixed", "1")
                        .output(Some("data"), &["id", "meta.name"])
                        .paginate(pagination),
                ),
            ],
        ))
        .properties(return_all_and_limit(None, &["list"]))
        .property(simplify_prop())
        .property(Prop::string("filter", "Filter").send(RouteSend::query("q")))
        .property(Prop::string("trace", "Trace").send(RouteSend::header("X-Trace")))
        .build();
    RestNode::new(
        desc,
        RestSpec::new("={{$credentials.baseUrl}}").credential("demoApi", true),
        &creds,
    )
}

fn ctx(url: &str, params: Value) -> NodeContext {
    let n: Node = serde_json::from_value(json!({
        "name": "R", "type": "hokkabaz.demoRest", "typeVersion": 1, "position": [0, 0],
        "parameters": params, "credentials": {"demoApi": {"id": "1", "name": "h"}}
    }))
    .unwrap();
    let mut c = NodeContext::new(n, vec![vec![Item::from_json(json!({"kind": "box"}))]], 0);
    c.hooks.credentials = Some(Arc::new(Creds(json!({"baseUrl": url, "key": "K_SECRET"}))));
    c
}

fn page(ids: &[u32]) -> String {
    let data: Vec<Value> = ids
        .iter()
        .map(|i| json!({"id": i, "meta": {"name": format!("n{i}"), "x": 1}, "junk": true}))
        .collect();
    http_resp("200 OK", &json!({"data": data}).to_string())
}

fn line(req: &str) -> &str {
    req.lines().next().unwrap()
}

#[tokio::test]
async fn offset_pagination_return_all_with_simplify() {
    let n = node(Pagination::Offset {
        limit_param: "limit".into(),
        offset_param: "offset".into(),
        page_size: 2,
    });
    let (url, seen) = serve(vec![page(&[1, 2]), page(&[3])]).await;
    let out = n
        .execute(&ctx(
            &url,
            json!({"operation": "list", "returnAll": true, "filter": "a b", "trace": "t1"}),
        ))
        .await
        .unwrap();
    let ids: Vec<_> = out[0]
        .iter()
        .map(|i| i.json["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, vec![1, 2, 3]);
    // simplify default true: only listed fields, nested kept
    assert_eq!(
        Value::Object(out[0][0].json.clone()),
        json!({"id": 1, "meta": {"name": "n1"}})
    );

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    // expression in the route url resolved against the item
    assert!(
        line(&seen[0]).starts_with("GET /things/box?"),
        "{}",
        line(&seen[0])
    );
    for q in ["fixed=1", "q=a+b", "limit=2", "offset=0"] {
        assert!(line(&seen[0]).contains(q), "{q} in {}", line(&seen[0]));
    }
    assert!(line(&seen[1]).contains("offset=2"));
    let low = seen[0].to_lowercase();
    assert!(low.contains("x-trace: t1") && low.contains("k_secret"));
}

#[tokio::test]
async fn limit_stops_paging_and_simplify_off_keeps_raw() {
    let n = node(Pagination::Page {
        page_param: "page".into(),
        size_param: "per_page".into(),
        page_size: 2,
        first_page: 1,
    });
    let (url, seen) = serve(vec![page(&[1, 2]), page(&[3, 4])]).await;
    let out = n
        .execute(&ctx(
            &url,
            json!({"operation": "list", "returnAll": false, "limit": 3, "simplify": false}),
        ))
        .await
        .unwrap();
    assert_eq!(out[0].len(), 3);
    assert_eq!(out[0][0].json["junk"], true);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    // last page asks only for what is still missing
    assert!(
        line(&seen[1]).contains("per_page=1") && line(&seen[1]).contains("page=2"),
        "{}",
        line(&seen[1])
    );
}

#[tokio::test]
async fn cursor_and_next_url_pagination() {
    let cur = node(Pagination::Cursor {
        cursor_param: "after".into(),
        next_path: "next".into(),
        size_param: None,
        page_size: 10,
    });
    let p1 = http_resp("200 OK", r#"{"data":[{"id":1}],"next":"c2"}"#);
    let p2 = http_resp("200 OK", r#"{"data":[{"id":2}],"next":""}"#);
    let (url, seen) = serve(vec![p1, p2]).await;
    let out = cur
        .execute(&ctx(&url, json!({"operation": "list", "returnAll": true})))
        .await
        .unwrap();
    assert_eq!(out[0].len(), 2);
    assert!(line(&seen.lock().unwrap()[1]).contains("after=c2"));

    let nu = node(Pagination::NextUrl {
        next_path: "links.next".into(),
    });
    let p1 = http_resp(
        "200 OK",
        r#"{"data":[{"id":1}],"links":{"next":"/things/box?page=2"}}"#,
    );
    let p2 = http_resp("200 OK", r#"{"data":[{"id":2}],"links":{}}"#);
    let (url, seen) = serve(vec![p1, p2]).await;
    let out = nu
        .execute(&ctx(&url, json!({"operation": "list", "returnAll": true})))
        .await
        .unwrap();
    assert_eq!(out[0].len(), 2);
    assert!(line(&seen.lock().unwrap()[1]).starts_with("GET /things/box?page=2 "));
}

#[tokio::test]
async fn unknown_operation_and_missing_credential() {
    let n = node(Pagination::NextUrl {
        next_path: "n".into(),
    });
    let e = n
        .execute(&ctx("http://x", json!({"operation": "nope"})))
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("Unknown operation"), "{e}");

    let mut c = ctx("http://x", json!({"operation": "list"}));
    c.node.credentials = None;
    let e = n.execute(&c).await.unwrap_err().to_string();
    assert!(e.contains("No credentials of type `demoApi`"), "{e}");
    assert_eq!(n.description().name, "hokkabaz.demoRest");
}
