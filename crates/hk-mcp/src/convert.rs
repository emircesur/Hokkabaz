//! rmcp model -> hk types. Goes through serde JSON on purpose: the wire shape (camelCase) is the
//! stable contract, rmcp's Rust structs are `#[non_exhaustive]` and churn between releases.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::McpError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfo {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Vec<PromptArgument>,
}

/// One rendered prompt message (`prompts/get`). `text` is flattened content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptMessageInfo {
    pub role: String,
    pub text: String,
}

pub(crate) fn via_json<S: Serialize, T: for<'de> Deserialize<'de>>(v: &S) -> Result<T, McpError> {
    serde_json::to_value(v)
        .and_then(serde_json::from_value)
        .map_err(|e| McpError::Request(format!("unexpected mcp payload: {e}")))
}

pub(crate) fn list_via_json<S: Serialize, T: for<'de> Deserialize<'de>>(
    items: &[S],
) -> Result<Vec<T>, McpError> {
    items.iter().map(via_json).collect()
}

/// Flatten MCP content blocks to model-visible text. Binary payloads are described, never
/// inlined (base64 would blow the context and is useless to a text model).
pub(crate) fn content_to_text(blocks: &Value) -> String {
    let Some(arr) = blocks.as_array() else {
        return String::new();
    };
    let parts: Vec<String> = arr.iter().map(block_to_text).collect();
    parts.join("\n")
}

fn block_to_text(b: &Value) -> String {
    let s = |k: &str| b.get(k).and_then(Value::as_str);
    match s("type") {
        Some("text") => s("text").unwrap_or_default().to_owned(),
        Some("image") => format!("[image: {}]", s("mimeType").unwrap_or("unknown")),
        Some("audio") => format!("[audio: {}]", s("mimeType").unwrap_or("unknown")),
        Some("resource_link") => format!("[resource_link: {}]", s("uri").unwrap_or("")),
        Some("resource") => {
            let r = b.get("resource").unwrap_or(&Value::Null);
            let uri = r.get("uri").and_then(Value::as_str).unwrap_or("");
            match r.get("text").and_then(Value::as_str) {
                Some(t) => t.to_owned(),
                None => format!("[resource: {uri}]"),
            }
        }
        _ => b.to_string(),
    }
}

/// `tools/call` result -> (text, is_error). Falls back to structured content when the server
/// sent no unstructured blocks.
pub(crate) fn call_result_to_text(result: &Value) -> (String, bool) {
    let mut text = result
        .get("content")
        .map(content_to_text)
        .unwrap_or_default();
    if text.is_empty()
        && let Some(sc) = result.get("structuredContent")
        && !sc.is_null()
    {
        text = sc.to_string();
    }
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    (text, is_error)
}

pub(crate) fn prompt_messages(result: &Value) -> Vec<PromptMessageInfo> {
    result
        .get("messages")
        .and_then(Value::as_array)
        .map(|ms| {
            ms.iter()
                .map(|m| PromptMessageInfo {
                    role: m
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("user")
                        .to_owned(),
                    text: m.get("content").map(block_to_text).unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `resources/read` result -> text of all contents (blobs described).
pub(crate) fn resource_contents_text(result: &Value) -> String {
    let Some(arr) = result.get("contents").and_then(Value::as_array) else {
        return String::new();
    };
    arr.iter()
        .map(|c| match c.get("text").and_then(Value::as_str) {
            Some(t) => t.to_owned(),
            None => format!(
                "[binary resource: {} {}]",
                c.get("uri").and_then(Value::as_str).unwrap_or(""),
                c.get("mimeType").and_then(Value::as_str).unwrap_or("")
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn call_result_text_and_error() {
        let r = json!({"content":[{"type":"text","text":"a"},{"type":"image","data":"xx","mimeType":"image/png"}],"isError":true});
        assert_eq!(
            call_result_to_text(&r),
            ("a\n[image: image/png]".into(), true)
        );
    }

    #[test]
    fn structured_fallback() {
        let r = json!({"content":[],"structuredContent":{"k":1}});
        assert_eq!(call_result_to_text(&r), (r#"{"k":1}"#.into(), false));
    }

    #[test]
    fn embedded_resource_and_prompt() {
        let r = json!({"messages":[{"role":"assistant","content":{"type":"resource","resource":{"uri":"u","text":"hi"}}}]});
        assert_eq!(
            prompt_messages(&r),
            vec![PromptMessageInfo {
                role: "assistant".into(),
                text: "hi".into()
            }]
        );
    }
}
