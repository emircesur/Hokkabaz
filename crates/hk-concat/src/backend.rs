use std::fmt;

use async_trait::async_trait;
use serde_json::Value;

use crate::ConcatError;

/// One embedded image from a tool reply (e.g. an inline preview).
#[derive(Debug, Clone, PartialEq)]
pub struct ReplyImage {
    pub mime: String,
    /// Base64, as sent by Concat.
    pub data: String,
}

/// What a Concat tool call returned.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolReply {
    /// The reply as JSON (`structuredContent`, else the text parsed as JSON, else a string).
    pub value: Value,
    /// Raw text content.
    pub text: String,
    pub images: Vec<ReplyImage>,
}

/// Where a backend runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    Process,
    Engine,
}

/// A Concat editor behind the 40-odd tools of Concat's MCP surface (docs/MCP.md). Process and
/// in-process engine implement this identically, so [`crate::ConcatClient`] and the agent tools
/// do not care which one runs. Tool names and argument shapes are Concat's own (camelCase fields).
#[async_trait]
pub trait ConcatBackend: Send + Sync + fmt::Debug {
    fn kind(&self) -> BackendType;

    /// Calls Concat tool `name`. A tool-level refusal is `Err(ConcatError::Tool | OutsideRoots)`.
    async fn call_tool(&self, name: &str, args: Value) -> Result<ToolReply, ConcatError>;

    /// Stops the editor (closes the child's stdin and reaps it). Idempotent.
    async fn shutdown(&self) {}
}
