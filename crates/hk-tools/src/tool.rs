use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// Tool schema offered to the model. Mirrors `hk_llm::ToolSpec` field-for-field (kept local so
/// this crate does not depend on the provider layer); hk-agent converts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

/// How much scrutiny a call needs. Approval policy keys off this (`>= Ask` emits approval).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Safe,
    Ask,
    Dangerous,
}

/// Per-call context handed to `Tool::execute`.
#[derive(Debug, Clone, Default)]
pub struct ToolCtx {
    pub call_id: String,
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
}

/// Result of a tool. `content` is what the model sees (after redaction/truncation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
    /// Set by the pipeline when `content` was cut and the full text spilled to this path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spill_path: Option<PathBuf>,
    /// Byte length of the output before truncation (only set when truncated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_bytes: Option<usize>,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            spill_path: None,
            original_bytes: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::ok(content)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool `{0}`")]
    Unknown(String),
    #[error("tool `{0}` already registered")]
    Duplicate(String),
    #[error("invalid schema for tool `{tool}`: {reason}")]
    BadSchema { tool: String, reason: String },
    #[error("invalid arguments for `{tool}`: {reason}")]
    InvalidArgs { tool: String, reason: String },
    #[error("`{tool}` blocked: {reason}")]
    Rejected { tool: String, reason: String },
    #[error("`{tool}` failed: {reason}")]
    Failed { tool: String, reason: String },
}

impl ToolError {
    /// Text safe to feed back to the model as an error tool result.
    pub fn to_output(&self) -> ToolOutput {
        ToolOutput::error(self.to_string())
    }
}

#[async_trait]
pub trait Tool: Send + Sync + 'static {
    /// Name, description, JSON-Schema `parameters`.
    fn spec(&self) -> ToolSpec;

    fn risk(&self, _args: &Value) -> Risk {
        Risk::Safe
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError>;
}
