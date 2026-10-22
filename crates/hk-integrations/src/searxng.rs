//! SearXNG (`hokkabaz.searxng`): meta-search through a self-hosted instance's JSON API, as a
//! flow node (one item per result) and as the `web_search` agent tool.
//!
//! Only the configured base URL is ever contacted. The instance must have the `json` format
//! enabled (`search.formats` in settings.yml), otherwise it answers 403.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Url;
use serde_json::{Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};
use hk_tools::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop};
use crate::kit::{self, node_err};
use crate::template::display;

pub const NODE_TYPE: &str = "hokkabaz.searxng";
pub const CREDENTIAL: &str = "searxngApi";
pub const TOOL_NAME: &str = "web_search";

const TIMEOUT: Duration = Duration::from_secs(20);
/// Tool output cap (bytes of text handed to the model).
const MAX_TOOL_OUTPUT: usize = 8_000;
const DEFAULT_TOOL_RESULTS: usize = 8;
const MAX_TOOL_RESULTS: usize = 20;
/// Fields kept from each SearXNG result (the raw object also carries parsed_url, template, ...).
const KEEP: [&str; 8] = ["title", "url", "content", "engine", "engines", "category", "score", "publishedDate"];

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "SearXNG API")
        .documentation("https://docs.searxng.org/dev/search_api.html")
        .property(Prop::string("baseUrl", "Base URL").required().placeholder("https://searx.example.com"))
        .property(
            Prop::string("apiKey", "Access Token")
                .password()
                .description("Optional bearer token, for instances behind an authenticating proxy"),
        )
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SearchQuery {
    pub query: String,
    pub categories: Option<String>,
    pub engines: Option<String>,
    pub language: Option<String>,
    pub time_range: Option<String>,
    pub pageno: Option<u32>,
    pub safesearch: Option<u8>,
}

/// `<base>/search?q=...&format=json&...`; errors are plain strings (the tool has no node).
pub(crate) fn search_url(base: &str, q: &SearchQuery) -> Result<Url, String> {
    let mut url = Url::parse(base.trim()).map_err(|_| "The SearXNG base URL is not a valid URL".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("The SearXNG base URL must be http or https".into());
    }
    url.path_segments_mut().map_err(|()| "The SearXNG base URL cannot be a base".to_owned())?.pop_if_empty().push("search");
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("q", &q.query).append_pair("format", "json");
        let opt = [
            ("categories", &q.categories),
            ("engines", &q.engines),
            ("language", &q.language),
            ("time_range", &q.time_range),
        ];
        for (k, v) in opt {
            if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                qp.append_pair(k, v);
            }
        }
        if let Some(p) = q.pageno.filter(|p| *p > 1) {
            qp.append_pair("pageno", &p.to_string());
        }
        if let Some(s) = q.safesearch {
            qp.append_pair("safesearch", &s.to_string());
        }
    }
    Ok(url)
}

/// Results of one query page, each cut down to [`KEEP`].
pub(crate) async fn search(
    http: &reqwest::Client,
    base: &str,
    token: Option<&str>,
    q: &SearchQuery,
) -> Result<Vec<Value>, String> {
    if q.query.trim().is_empty() {
        return Err("The search query is empty".into());
    }
    let mut rb = http.get(search_url(base, q)?).header("accept", "application/json");
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        rb = rb.bearer_auth(t);
    }
    let res = rb.send().await.map_err(|e| format!("The request failed: {}", e.without_url()))?;
    let status = res.status();
    let body = res.text().await.map_err(|e| format!("Could not read the response: {}", e.without_url()))?;
    if !status.is_success() {
        let hint = if status.as_u16() == 403 { " (is the `json` format enabled in the instance's settings.yml?)" } else { "" };
        return Err(format!("SearXNG returned {}{hint}", status.as_u16()));
    }
    let parsed: Value = serde_json::from_str(&body).map_err(|_| "SearXNG did not return JSON".to_owned())?;
    Ok(parsed
        .get("results")
        .and_then(Value::as_array)
        .map(|rs| {
            rs.iter()
                .map(|r| {
                    Value::Object(KEEP.iter().filter_map(|k| r.get(*k).map(|v| ((*k).to_owned(), v.clone()))).collect())
                })
                .collect()
        })
        .unwrap_or_default())
}

// ------------------------------------------------------------------------------- node

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "SearXNG")
        .description("Search the web through a SearXNG instance")
        .subtitle("={{ $parameter.query }}")
        .icon("fa:search")
        .color("#3050ff")
        .credential(CREDENTIAL, true)
        .property(Prop::string("query", "Query").required())
        .property(Prop::number("limit", "Limit").default(10).min(1.0).description("Max number of results to return"))
        .property(Prop::collection(
            "options",
            "Options",
            vec![
                Prop::string("categories", "Categories").placeholder("general,news").description("Comma-separated"),
                Prop::string("engines", "Engines").description("Comma-separated; default: the instance's"),
                Prop::string("language", "Language").placeholder("en"),
                Prop::options(
                    "timeRange",
                    "Time Range",
                    vec![Choice::new("Any", ""), Choice::new("Day", "day"), Choice::new("Month", "month"), Choice::new("Year", "year")],
                ),
                Prop::number("pageNo", "Page").default(1).min(1.0),
                Prop::options(
                    "safeSearch",
                    "Safe Search",
                    vec![Choice::new("Off", 0), Choice::new("Moderate", 1), Choice::new("Strict", 2)],
                ),
            ],
        ))
        .build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

#[derive(Debug)]
struct SearxngNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

impl SearxngNode {
    fn query(&self, ctx: &NodeContext, i: usize) -> Result<SearchQuery, FlowError> {
        let opt = |name: &str| kit::field_str(ctx, &format!("options.{name}"), i);
        Ok(SearchQuery {
            query: kit::str_param(ctx, &self.desc, "query", i)?,
            categories: opt("categories")?,
            engines: opt("engines")?,
            language: opt("language")?,
            time_range: opt("timeRange")?,
            pageno: opt("pageNo")?.and_then(|p| p.parse().ok()),
            safesearch: opt("safeSearch")?.and_then(|p| p.parse().ok()),
        })
    }
}

#[async_trait]
impl FlowNode for SearxngNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let input_len = ctx.input.first().map_or(0, Vec::len);
        if input_len == 0 {
            return Ok(vec![Vec::new()]);
        }
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let (base, token) = (kit::cred_str(&cred, "baseUrl"), kit::cred_str(&cred, "apiKey"));
        let http = kit::client(&ctx.node.name, TIMEOUT)?;
        let mut out = Vec::new();
        for i in 0..input_len {
            let res = async {
                let limit = kit::param(ctx, &self.desc, "limit", i)?
                    .and_then(|v| v.as_u64().or_else(|| display(&v).parse().ok()))
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n > 0)
                    .unwrap_or(10);
                let rs = search(&http, &base, Some(&token), &self.query(ctx, i)?)
                    .await
                    .map_err(|m| node_err(ctx, m))?;
                Ok(rs.into_iter().take(limit).map(kit::to_item).collect::<Vec<Item>>())
            }
            .await;
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-searxng`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(SearxngNode { desc: description(), creds: creds.clone() }))
}

// ------------------------------------------------------------------------------- tool

struct WebSearch {
    base: String,
    token: Option<String>,
    http: OnceLock<Result<reqwest::Client, String>>,
}

impl std::fmt::Debug for WebSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // the token stays out of Debug output
        f.debug_struct("WebSearch").field("base", &self.base).finish_non_exhaustive()
    }
}

/// Cut `s` to at most `max` bytes on a char boundary, marking the cut.
fn truncate(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push_str("\n[truncated]");
    s
}

fn format_results(results: &[Value], max_results: usize) -> String {
    let field = |r: &Value, k: &str| r.get(k).map(display).unwrap_or_default();
    let mut out = String::new();
    for (n, r) in results.iter().take(max_results).enumerate() {
        out.push_str(&format!("{}. {}\n   {}\n", n + 1, field(r, "title"), field(r, "url")));
        let snippet = field(r, "content");
        if !snippet.is_empty() {
            let snippet: String = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push_str(&format!("   {}\n", snippet.chars().take(400).collect::<String>()));
        }
    }
    truncate(out, MAX_TOOL_OUTPUT)
}

#[async_trait]
impl Tool for WebSearch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TOOL_NAME.into(),
            description: "Search the web (SearXNG). Returns numbered results with title, URL and a snippet. \
                          Follow up with web_fetch to read a page."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "categories": {"type": "string", "description": "Comma-separated, e.g. general,news,it,science"},
                    "language": {"type": "string", "description": "e.g. en, de"},
                    "time_range": {"type": "string", "enum": ["day", "month", "year"]},
                    "page": {"type": "integer", "minimum": 1},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_TOOL_RESULTS}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        }
    }

    // the endpoint is operator-configured; the query is the only thing leaving the machine
    fn risk(&self, _args: &Value) -> Risk {
        Risk::Safe
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let invalid = |reason: &str| ToolError::InvalidArgs { tool: TOOL_NAME.into(), reason: reason.into() };
        let query = args.get("query").and_then(Value::as_str).map(str::trim).filter(|q| !q.is_empty()).ok_or_else(|| invalid("`query` is required"))?;
        let opt = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_owned);
        let q = SearchQuery {
            query: query.to_owned(),
            categories: opt("categories"),
            language: opt("language"),
            time_range: opt("time_range").filter(|t| matches!(t.as_str(), "day" | "month" | "year")),
            pageno: args.get("page").and_then(Value::as_u64).and_then(|p| u32::try_from(p).ok()),
            ..SearchQuery::default()
        };
        let max = args
            .get("max_results")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .map_or(DEFAULT_TOOL_RESULTS, |n| n.clamp(1, MAX_TOOL_RESULTS));
        let http = match self.http.get_or_init(|| reqwest::Client::builder().timeout(TIMEOUT).build().map_err(|e| e.without_url().to_string())) {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutput::error(format!("web_search is unavailable: {e}"))),
        };
        match search(http, &self.base, self.token.as_deref(), &q).await {
            Ok(rs) if rs.is_empty() => Ok(ToolOutput::ok("No results.")),
            Ok(rs) => Ok(ToolOutput::ok(format_results(&rs, max))),
            Err(m) => Ok(ToolOutput::error(format!("web_search failed: {m}"))),
        }
    }
}

/// `web_search` tool over the SearXNG instance at `base_url`, for `AgentRuntime::add_tools`.
pub fn searxng_web_search_tool(base_url: impl Into<String>) -> Arc<dyn Tool> {
    searxng_web_search_tool_with_token(base_url, None)
}

/// Like [`searxng_web_search_tool`], sending `token` as a bearer token.
pub fn searxng_web_search_tool_with_token(base_url: impl Into<String>, token: Option<String>) -> Arc<dyn Tool> {
    Arc::new(WebSearch { base: base_url.into(), token, http: OnceLock::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_carries_only_set_options() {
        let q = SearchQuery { query: "rust & co".into(), categories: Some("news".into()), pageno: Some(2), ..SearchQuery::default() };
        assert_eq!(
            search_url("https://s.example/base/", &q).unwrap().as_str(),
            "https://s.example/base/search?q=rust+%26+co&format=json&categories=news&pageno=2"
        );
        assert!(search_url("ftp://x", &q).is_err());
        assert!(search_url("nonsense", &q).is_err());
    }

    #[test]
    fn output_is_capped_on_char_boundaries() {
        let results: Vec<Value> = (0..20).map(|i| json!({"title": format!("T{i}"), "url": "https://x", "content": "é".repeat(390)})).collect();
        let s = format_results(&results, 20);
        assert!(s.len() <= MAX_TOOL_OUTPUT + "\n[truncated]".len());
        assert!(s.ends_with("[truncated]"));
        assert!(s.starts_with("1. T0\n   https://x\n"));
    }
}
