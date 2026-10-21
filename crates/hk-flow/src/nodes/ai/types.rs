//! Handle traits a sub-node supplies to its root, plus the host hooks the AI nodes use.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use hk_llm::{FinishReason, Message, ToolCall, ToolSpec, Usage};
use serde_json::{Map, Value};

use crate::error::FlowError;

/// Failure inside an AI handle. The executor reports only the message, so the node
/// name is a placeholder the root node never shows.
pub(crate) fn ai_err(message: impl Into<String>) -> FlowError {
    FlowError::Node {
        node: "ai".into(),
        message: message.into(),
    }
}

// ------------------------------------------------------------------ chat model

/// One finished model turn (the stream, aggregated).
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish: FinishReason,
    pub usage: Usage,
}

/// `ai_languageModel` handle. Streaming is not exposed: n8n nodes consume whole turns.
#[async_trait]
pub trait ChatModel: Send + Sync + fmt::Debug {
    fn model_name(&self) -> &str;

    /// One request. `tools` empty = plain completion.
    async fn complete(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolSpec>,
    ) -> Result<Completion, FlowError>;
}

/// What a chat-model node asks the host to build. Secrets ride along only until the
/// factory has built the provider; the redacted `Debug` keeps them out of logs.
#[derive(Clone, Default)]
pub struct ChatModelSpec {
    /// `openai`, `anthropic`, `deepseek`, `gemini`, `ollama`, `openrouter`.
    pub provider: String,
    pub model: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u64>,
    /// Endpoint override from the credential or node options.
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

impl fmt::Debug for ChatModelSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatModelSpec")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Host seam that turns a [`ChatModelSpec`] into a model. The default
/// ([`HkLlmChatModelFactory`](super::model::HkLlmChatModelFactory)) builds hk-llm
/// providers; the server may swap in one backed by its own provider registry, tests use
/// the scripted fake.
#[async_trait]
pub trait ChatModelFactory: Send + Sync + fmt::Debug {
    async fn build(&self, spec: ChatModelSpec) -> Result<Arc<dyn ChatModel>, FlowError>;
}

// --------------------------------------------------------------------- memory

/// `ai_memory` handle: conversation history of one session.
#[async_trait]
pub trait Memory: Send + Sync + fmt::Debug {
    /// Windowed history, oldest first (user/assistant messages only).
    async fn load(&self) -> Vec<Message>;
    /// Append one finished exchange.
    async fn save(&self, user: &str, assistant: &str);
    async fn clear(&self);
}

/// One remembered message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEntry {
    pub role: String,
    pub content: String,
}

/// Process-local chat history keyed by `<workflow>:<node>:<session>`. Shared by every
/// execution of one executor (hooks are cloned per run, the `Arc` is shared), so a chat
/// continues across executions without a database. Not persisted across restarts.
#[derive(Debug, Clone, Default)]
pub struct ChatMemoryStore(Arc<Mutex<BTreeMap<String, Vec<MemoryEntry>>>>);

impl ChatMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Vec<MemoryEntry> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn push(&self, key: &str, entries: impl IntoIterator<Item = MemoryEntry>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(key.to_owned())
            .or_default()
            .extend(entries);
    }

    pub fn clear(&self, key: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
    }
}

// ----------------------------------------------------------------------- tools

/// `ai_tool` handle.
#[async_trait]
pub trait AgentTool: Send + Sync + fmt::Debug {
    /// Name the model calls (`[A-Za-z0-9_-]{1,64}`).
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON Schema of the arguments object.
    fn schema(&self) -> Value;
    /// Tool output as text. An `Err` is fed back to the model, not raised.
    async fn call(&self, args: Value) -> Result<String, FlowError>;
}

impl dyn AgentTool {
    pub fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_owned(),
            description: self.description().to_owned(),
            parameters: self.schema(),
        }
    }
}

// --------------------------------------------------------------- output parser

/// `ai_outputParser` handle.
pub trait OutputParser: Send + Sync + fmt::Debug {
    /// Text appended to the prompt so the model emits the expected shape.
    fn format_instructions(&self) -> String;
    /// Parse the model's final text.
    fn parse(&self, text: &str) -> Result<Value, FlowError>;
}

// ------------------------------------------------------------- native agent hook

/// Request for the full Hokkabaz agent (`hokkabaz.agent`).
#[derive(Debug, Clone, Default)]
pub struct AgentRunRequest {
    pub prompt: String,
    pub system: Option<String>,
    /// Session to continue; `None` = fresh session.
    pub session_id: Option<String>,
    /// Agent mode (`build`, `plan`, ...).
    pub mode: Option<String>,
    /// `provider/model` ref; `None` = host default.
    pub model: Option<String>,
    /// Tool allow-list; empty = the mode's defaults.
    pub tools: Vec<String>,
    pub max_steps: Option<u32>,
    pub workflow_id: Option<String>,
    pub execution_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentRunResult {
    pub output: String,
    pub session_id: Option<String>,
    /// Tool calls / observations, already free of secrets.
    pub steps: Vec<Value>,
    pub extra: Map<String, Value>,
}

/// Seam to the hk-agent loop. hk-flow does not depend on hk-agent (tools, sandbox and
/// session store stay out of the workflow engine); the server/FFI implements this.
#[async_trait]
pub trait AgentRunner: Send + Sync + fmt::Debug {
    async fn run(&self, req: AgentRunRequest) -> Result<AgentRunResult, FlowError>;
}
