use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{fail, parse};
use crate::tool::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};

const MAX_BODY: usize = 5 * 1024 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Default)]
pub struct WebFetchTool {
    client: std::sync::OnceLock<reqwest::Client>,
}

#[derive(Deserialize)]
struct Args {
    url: String,
    timeout_secs: Option<u64>,
}

#[async_trait]
impl Tool for WebFetchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_fetch".into(),
            description: "GET an http(s) URL and return its text. HTML is reduced to plain text. Max 5 MB body.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "timeout_secs": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS}
                },
                "required": ["url"],
                "additionalProperties": false
            }),
        }
    }

    fn risk(&self, _: &Value) -> Risk {
        Risk::Ask
    }

    async fn execute(&self, _: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = parse("web_fetch", args)?;
        if !(a.url.starts_with("http://") || a.url.starts_with("https://")) {
            return fail("only http:// and https:// URLs are supported");
        }
        let client = match self.client.get() {
            Some(c) => c.clone(),
            None => match reqwest::Client::builder()
                .user_agent("hokkabaz/0")
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
            {
                Ok(c) => self.client.get_or_init(|| c).clone(),
                Err(e) => return fail(format!("http client init failed: {e}")),
            },
        };
        let timeout = Duration::from_secs(
            a.timeout_secs
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .min(MAX_TIMEOUT_SECS),
        );
        let mut resp = match client.get(&a.url).timeout(timeout).send().await {
            Ok(r) => r,
            Err(e) => return fail(format!("request failed: {e}")),
        };
        let status = resp.status();
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let textual = ctype.is_empty()
            || ctype.starts_with("text/")
            || ["json", "xml", "javascript", "yaml"]
                .iter()
                .any(|k| ctype.contains(k));
        if !textual {
            return fail(format!(
                "unsupported content-type `{ctype}` (HTTP {status})"
            ));
        }
        let mut body = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(c)) => {
                    body.extend_from_slice(&c);
                    if body.len() > MAX_BODY {
                        return fail(format!("response exceeds {MAX_BODY} bytes"));
                    }
                }
                Ok(None) => break,
                Err(e) => return fail(format!("read failed: {e}")),
            }
        }
        let mut text = String::from_utf8_lossy(&body).into_owned();
        if ctype.contains("html") {
            text = html_to_text(&text);
        }
        if !status.is_success() {
            return Ok(ToolOutput::error(format!("HTTP {status}\n{text}")));
        }
        Ok(ToolOutput::ok(text))
    }
}

/// Crude tag stripper: drops script/style bodies and tags, decodes a few entities, collapses
/// blank runs. Good enough for the model to read docs pages; not a renderer.
fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0;
    let b = html.as_bytes();
    while i < b.len() {
        if b[i] == b'<' {
            let skip_to = ["script", "style"].iter().find_map(|t| {
                lower[i..].starts_with(&format!("<{t}")).then(|| {
                    lower[i..]
                        .find(&format!("</{t}"))
                        .map_or(b.len(), |p| i + p)
                })
            });
            if let Some(j) = skip_to {
                i = j;
            }
            let end = html[i..].find('>').map_or(b.len(), |p| i + p + 1);
            let tag = &lower[i..end.min(lower.len())];
            if [
                "<br", "<p", "</p", "<div", "</div", "<li", "<h", "</h", "<tr",
            ]
            .iter()
            .any(|t| tag.starts_with(t))
            {
                out.push('\n');
            }
            i = end;
        } else {
            let next = html[i..].find('<').map_or(b.len(), |p| i + p);
            out.push_str(&html[i..next]);
            i = next;
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let mut res = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        res.push_str(line);
        res.push('\n');
    }
    res.trim().to_owned()
}
