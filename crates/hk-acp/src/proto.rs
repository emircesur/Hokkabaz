//! ACP wire types (protocol version 1), only the subset a client needs. Field names are
//! camelCase on the wire. Unknown fields are ignored so newer agents keep working.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: u32,
    pub client_capabilities: ClientCapabilities,
    pub client_info: Implementation,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    pub fs: FsCapabilities,
    pub terminal: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsCapabilities {
    pub read_text_file: bool,
    pub write_text_file: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Implementation {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: u32,
    #[serde(default)]
    pub agent_capabilities: Value,
    #[serde(default)]
    pub auth_methods: Vec<Value>,
    #[serde(default)]
    pub agent_info: Option<Implementation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionParams {
    pub cwd: String,
    pub mcp_servers: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResult {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptParams {
    pub session_id: String,
    pub prompt: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResult {
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

/// Content blocks. Only text is produced; other kinds are kept opaque on receive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    #[serde(other)]
    Other,
}

impl ContentBlock {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            Self::Other => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotification {
    pub session_id: String,
    pub update: SessionUpdate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl ToolStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallFields {
    pub tool_call_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: Option<ToolStatus>,
    /// Content blocks as sent (`content`, `diff`, `terminal` wrappers); see [`tool_content_text`].
    #[serde(default)]
    pub content: Option<Vec<Value>>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    UserMessageChunk { content: ContentBlock },
    AgentMessageChunk { content: ContentBlock },
    AgentThoughtChunk { content: ContentBlock },
    ToolCall(ToolCallFields),
    ToolCallUpdate(ToolCallFields),
    Plan { entries: Vec<PlanEntry> },
    /// available_commands_update, current_mode_update, future kinds: ignored.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    pub session_id: String,
    pub tool_call: ToolCallFields,
    pub options: Vec<PermissionOption>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    #[serde(default)]
    pub name: String,
    pub kind: PermissionOptionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

impl PermissionOptionKind {
    pub fn is_allow(self) -> bool {
        matches!(self, Self::AllowOnce | Self::AllowAlways)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTextFileParams {
    pub path: String,
    /// 1-based first line.
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteTextFileParams {
    pub path: String,
    pub content: String,
}

/// Flatten tool content blocks to text the model/user would read: text blocks verbatim, diffs
/// as a one-line summary, terminals as a placeholder.
pub fn tool_content_text(blocks: &[Value]) -> String {
    let mut parts = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("content") => {
                if let Some(t) = b.pointer("/content/text").and_then(Value::as_str) {
                    parts.push(t.to_owned());
                }
            }
            Some("diff") => {
                let path = b.get("path").and_then(Value::as_str).unwrap_or("?");
                parts.push(format!("diff {path}"));
            }
            Some("terminal") => parts.push("[terminal output]".to_owned()),
            _ => {}
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_update_kinds() {
        let n: SessionNotification = serde_json::from_value(json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}}
        }))
        .unwrap();
        assert!(matches!(n.update, SessionUpdate::AgentMessageChunk { content } if content.as_text() == Some("hi")));

        let n: SessionNotification = serde_json::from_value(json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Read", "kind": "read", "status": "pending"}
        }))
        .unwrap();
        assert!(matches!(n.update, SessionUpdate::ToolCall(t) if t.tool_call_id == "c1"));

        let n: SessionNotification = serde_json::from_value(json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "available_commands_update", "availableCommands": []}
        }))
        .unwrap();
        assert!(matches!(n.update, SessionUpdate::Other));
    }

    #[test]
    fn content_text_flattens() {
        let t = tool_content_text(&[
            json!({"type": "content", "content": {"type": "text", "text": "out"}}),
            json!({"type": "diff", "path": "/a", "oldText": null, "newText": "x"}),
        ]);
        assert_eq!(t, "out\ndiff /a");
    }
}
