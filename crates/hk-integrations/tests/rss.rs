//! RSS nodes against fixture feeds served by a local server.
#![cfg(feature = "int-rss")]

mod common;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::poll::{MemoryPollState, PollContext, run_poll};
use hk_integrations::{CredentialTypes, PollRegistry, register_integrations_with_polling};
use serde_json::{Value, json};

use common::serve;

const FEED: &str = include_str!("fixtures/rss/feed.xml");
const FEED_NEW: &str = include_str!("fixtures/rss/feed_new_item.xml");
const ATOM: &str = include_str!("fixtures/rss/atom.xml");

fn xml(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn setup() -> (NodeRegistry, PollRegistry) {
    let mut nodes = NodeRegistry::new();
    let mut polls = PollRegistry::new();
    register_integrations_with_polling(&mut nodes, &CredentialTypes::new(), &mut polls).unwrap();
    (nodes, polls)
}

fn node(ty: &str, params: Value) -> Node {
    serde_json::from_value(json!({
        "name": "RSS", "type": ty, "typeVersion": 1, "position": [0, 0], "parameters": params
    }))
    .unwrap()
}

#[tokio::test]
async fn read_returns_one_item_per_entry() {
    let (nodes, _p) = setup();
    let (url, seen) = serve(vec![xml(FEED)]).await;
    let c = NodeContext::new(
        node("n8n-nodes-base.rssFeedRead", json!({"url": format!("{url}/feed.xml")})),
        vec![vec![Item::from_json(json!({}))]],
        0,
    );
    let out = nodes.latest("n8n-nodes-base.rssFeedRead").unwrap().execute(&c).await.unwrap();
    let items = &out[0];
    assert_eq!(items.len(), 2);
    let a = &items[0].json;
    assert_eq!(a["title"], "Second post");
    assert_eq!(a["link"], "https://example.com/2");
    assert_eq!(a["guid"], "post-2");
    assert_eq!(a["creator"], "Ann");
    assert_eq!(a["categories"], json!(["news", "rust"]));
    assert_eq!(a["isoDate"], "2025-07-01T10:52:37+00:00");
    assert!(a["pubDate"].as_str().unwrap().contains("1 Jul 2025 10:52:37"), "{}", a["pubDate"]);
    assert_eq!(a["contentSnippet"], "Fish & chips");
    assert_eq!(items[1].json["guid"], "post-1");
    assert!(seen.lock().unwrap()[0].starts_with("GET /feed.xml HTTP/1.1"));
}

#[tokio::test]
async fn read_handles_atom_and_reports_failures() {
    let (nodes, _p) = setup();
    let n = nodes.latest("n8n-nodes-base.rssFeedRead").unwrap();
    let mk = |url: String| {
        NodeContext::new(
            node("n8n-nodes-base.rssFeedRead", json!({"url": url})),
            vec![vec![Item::from_json(json!({}))]],
            0,
        )
    };

    let (url, _s) = serve(vec![xml(ATOM)]).await;
    let out = n.execute(&mk(url)).await.unwrap();
    assert_eq!(out[0][0].json["title"], "Atom entry");
    assert_eq!(out[0][0].json["link"], "https://example.org/a1");
    assert_eq!(out[0][0].json["creator"], "Bea");

    let (url, _s) = serve(vec![xml("<html>not a feed</html>")]).await;
    let e = n.execute(&mk(url)).await.unwrap_err().to_string();
    assert!(e.contains("Could not parse the feed"), "{e}");

    let (url, _s) = serve(vec!["HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()]).await;
    let e = n.execute(&mk(url)).await.unwrap_err().to_string();
    assert!(e.contains("404"), "{e}");

    let e = n.execute(&mk("file:///etc/passwd".into())).await.unwrap_err().to_string();
    assert!(e.contains("http"), "{e}");
    let e = n.execute(&mk(String::new())).await.unwrap_err().to_string();
    assert!(e.contains("has to be set"), "{e}");
}

#[tokio::test]
async fn trigger_baselines_then_emits_only_new_guids() {
    let (_n, polls) = setup();
    let trig = polls.get("n8n-nodes-base.rssFeedReadTrigger").unwrap().clone();
    let (url, _s) = serve(vec![xml(FEED), xml(FEED_NEW), xml(FEED_NEW)]).await;
    let n = node("n8n-nodes-base.rssFeedReadTrigger", json!({"feedUrl": format!("{url}/f")}));
    let cx = PollContext::for_node(&n, Some("wf"), None);
    let store = MemoryPollState::default();

    let o = run_poll(trig.as_ref(), &store, &cx, "wf").await.unwrap();
    assert!(o.baseline && o.items.is_empty());
    let o = run_poll(trig.as_ref(), &store, &cx, "wf").await.unwrap();
    let titles: Vec<_> = o.items.iter().map(|i| i.json["title"].as_str().unwrap().to_owned()).collect();
    assert_eq!(titles, ["Third post"]);
    let o = run_poll(trig.as_ref(), &store, &cx, "wf").await.unwrap();
    assert!(o.items.is_empty());
}

#[test]
fn parse_feed_gives_stable_ids() {
    let a = hk_integrations::rss::parse_feed(FEED.as_bytes()).unwrap();
    assert_eq!(a.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["post-2", "post-1"]);
    // no guid: feed-rs derives one, and the same twice
    let bare = r#"<rss version="2.0"><channel><title>t</title><item><title>x</title><link>https://e.com/x</link></item></channel></rss>"#;
    let p = hk_integrations::rss::parse_feed(bare.as_bytes()).unwrap();
    let q = hk_integrations::rss::parse_feed(bare.as_bytes()).unwrap();
    assert!(!p[0].0.is_empty());
    assert_eq!(p[0].0, q[0].0);
}
