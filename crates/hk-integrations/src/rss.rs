//! RSS Feed Read (`n8n-nodes-base.rssFeedRead`) and RSS Feed Trigger
//! (`n8n-nodes-base.rssFeedReadTrigger`). RSS 0.x/1.0/2.0, Atom and JSON Feed through
//! feed-rs; one item per entry, shaped like n8n's rss-parser items.
//!
//! The trigger is a polling trigger that dedupes by guid (feed-rs derives a stable id when
//! the feed has none), so it works on top of the framework's seen-id window.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::CredentialTypes;
use crate::describe::{NodeBuilder, Prop, poll_times_prop};
use crate::kit::{self, err, node_err};
use crate::poll::{PollBatch, PollContext, PollRegistry, PolledItem, PollingTrigger};

pub const NODE_TYPE: &str = "n8n-nodes-base.rssFeedRead";
pub const TRIGGER_TYPE: &str = "n8n-nodes-base.rssFeedReadTrigger";

/// A feed larger than this is refused rather than buffered.
const MAX_FEED_BYTES: usize = 10 * 1024 * 1024;

fn read_description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "RSS Read")
        .description("Reads data from an RSS or Atom feed")
        .icon("fa:rss")
        .color("#b02a2a")
        .group(&["input"])
        .property(
            Prop::string("url", "URL")
                .required()
                .placeholder("https://example.com/feed.xml")
                .description("URL of the feed"),
        )
        .build();
    kit::serve_versions(&mut d, &[1.0, 1.1, 1.2]);
    d
}

fn trigger_description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(TRIGGER_TYPE, "RSS Feed Trigger")
        .description("Starts a workflow when a new feed entry appears")
        .icon("fa:rss")
        .color("#b02a2a")
        .polling_trigger()
        .property(poll_times_prop())
        .property(
            Prop::string("feedUrl", "Feed URL")
                .required()
                .placeholder("https://example.com/feed.xml"),
        )
        .build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

// ------------------------------------------------------------------ parse

/// Text with tags dropped and the common entities decoded (rss-parser's `contentSnippet`).
fn snippet(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse a feed document into `(guid, item json)` pairs in document order.
pub fn parse_feed(bytes: &[u8]) -> Result<Vec<(String, Value)>, String> {
    let feed =
        feed_rs::parser::parse(bytes).map_err(|e| format!("Could not parse the feed: {e}"))?;
    Ok(feed
        .entries
        .into_iter()
        .map(|e| {
            let mut m = serde_json::Map::new();
            let mut put = |k: &str, v: Value| {
                m.insert(k.to_owned(), v);
            };
            if let Some(t) = &e.title {
                put("title", json!(t.content));
            }
            if let Some(l) = e.links.first() {
                put("link", json!(l.href));
            }
            if let Some(d) = e.published.or(e.updated) {
                put("pubDate", json!(d.to_rfc2822()));
                put("isoDate", json!(d.to_rfc3339()));
            }
            if let Some(a) = e.authors.first() {
                put("creator", json!(a.name));
            }
            let body = e
                .content
                .as_ref()
                .and_then(|c| c.body.clone())
                .or_else(|| e.summary.as_ref().map(|s| s.content.clone()));
            if let Some(b) = body {
                put("contentSnippet", json!(snippet(&b)));
                put("content", json!(b));
            }
            put("guid", json!(e.id));
            if !e.categories.is_empty() {
                let cats: Vec<&str> = e.categories.iter().map(|c| c.term.as_str()).collect();
                put("categories", json!(cats));
            }
            (e.id, Value::Object(m))
        })
        .collect())
}

async fn fetch(node: &str, http: &reqwest::Client, url: &str) -> Result<Vec<u8>, FlowError> {
    let u = reqwest::Url::parse(url.trim())
        .map_err(|_| err(node, "The feed URL is not a valid URL"))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(err(node, "Only http and https feed URLs are supported"));
    }
    let res = http
        .get(u)
        .header(
            "accept",
            "application/rss+xml, application/atom+xml, application/xml, text/xml, */*",
        )
        .header("user-agent", "Hokkabaz")
        .send()
        .await
        .map_err(|e| err(node, format!("The request failed: {}", e.without_url())))?;
    let status = res.status();
    if !status.is_success() {
        return Err(err(
            node,
            format!(
                "The feed server returned an error: {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            ),
        ));
    }
    let bytes = res.bytes().await.map_err(|e| {
        err(
            node,
            format!("Could not read the feed: {}", e.without_url()),
        )
    })?;
    if bytes.len() > MAX_FEED_BYTES {
        return Err(err(node, "The feed is larger than 10 MB"));
    }
    Ok(bytes.to_vec())
}

// ------------------------------------------------------------------- read

#[derive(Debug)]
struct RssRead {
    desc: NodeTypeDescription,
}

#[async_trait]
impl FlowNode for RssRead {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let input_len = ctx.input.first().map_or(0, Vec::len);
        if input_len == 0 {
            return Ok(vec![Vec::new()]);
        }
        let http = kit::client(&ctx.node.name, Duration::from_secs(30))?;
        let mut out = Vec::new();
        for i in 0..input_len {
            let res = async {
                let url = kit::str_param(ctx, &self.desc, "url", i)?;
                if url.trim().is_empty() {
                    return Err(node_err(ctx, "The parameter 'URL' has to be set"));
                }
                let bytes = fetch(&ctx.node.name, &http, &url).await?;
                let entries = parse_feed(&bytes).map_err(|m| node_err(ctx, m))?;
                Ok(entries
                    .into_iter()
                    .map(|(_, v)| kit::to_item(v))
                    .collect::<Vec<Item>>())
            }
            .await;
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

// ---------------------------------------------------------------- trigger

#[derive(Debug)]
struct RssTrigger {
    desc: NodeTypeDescription,
}

#[async_trait]
impl PollingTrigger for RssTrigger {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn poll(
        &self,
        cx: &PollContext,
        _cursor: Option<&Value>,
    ) -> Result<PollBatch, FlowError> {
        let node = cx.node().name.clone();
        let url = cx.param_str("feedUrl", "")?;
        if url.trim().is_empty() {
            return Err(err(&node, "The parameter 'Feed URL' has to be set"));
        }
        let http = kit::client(&node, Duration::from_secs(30))?;
        let bytes = fetch(&node, &http, &url).await?;
        let entries = parse_feed(&bytes).map_err(|m| err(&node, m))?;
        Ok(PollBatch {
            items: entries
                .into_iter()
                .map(|(id, v)| PolledItem::new(id, v))
                .collect(),
            cursor: None,
        })
    }
}

/// Nodes into the shared registries. No credentials. Features: `int-rss`.
pub fn register(
    nodes: &mut NodeRegistry,
    _creds: &CredentialTypes,
    polls: &mut PollRegistry,
) -> Result<(), FlowError> {
    nodes.register(Arc::new(RssRead {
        desc: read_description(),
    }))?;
    polls.register(
        nodes,
        Arc::new(RssTrigger {
            desc: trigger_description(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_strips_markup() {
        assert_eq!(
            snippet("<p>Fish &amp; <b>chips</b></p>\n<p>more</p>"),
            "Fish & chips more"
        );
    }
}
