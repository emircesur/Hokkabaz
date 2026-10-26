//! Provider-neutral request types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Conversation role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// System prompt section.
    System,
    /// User input.
    User,
    /// Model output.
    Assistant,
    /// Tool result.
    Tool,
}

/// A tool invocation requested by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned call id; echoed back in the tool result message.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Arguments as JSON (parsed once the stream has finished the call).
    pub arguments: Value,
}

/// One message in the prompt. Always a projection of the session log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Author role.
    pub role: Role,
    /// Text content (tool output for [`Role::Tool`]).
    pub content: String,
    /// Prior reasoning text, replayed only for providers that require it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Tool calls made by an assistant message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// For [`Role::Tool`]: id of the call this answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Images attached to a user message (vision models).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImagePart>,
}

/// An inline image (base64), e.g. a pasted screenshot or a camera photo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePart {
    /// `image/png`, `image/jpeg`, …
    pub mime: String,
    /// Base64 (standard alphabet, no data: prefix).
    pub data: String,
}

impl ImagePart {
    /// `data:` URL form some APIs want.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime, self.data)
    }
}

/// Tool schema offered to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique tool name.
    pub name: String,
    /// What the tool does, shown to the model.
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

/// Whether and how the model may call tools.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    /// Model decides.
    #[default]
    Auto,
    /// Tools disabled for this request.
    None,
    /// Model must call some tool.
    Required,
    /// Model must call the named tool.
    Named(String),
}

/// Requested reasoning budget; adapters map this to their native knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// Minimal thinking.
    Low,
    /// Default thinking.
    Medium,
    /// Maximum thinking.
    High,
}

/// Sampling, reasoning and output knobs beyond the core three on
/// [`ChatRequest`]. Unset = provider default. Adapters serialize only what the
/// target accepts and drop the rest (see `crate::params`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SamplingParams {
    /// Nucleus sampling mass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Sample from the k most likely tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Drop tokens below `min_p` x the top token's probability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f64>,
    /// Penalty for tokens already present (OpenAI semantics).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    /// Penalty scaled by token frequency (OpenAI semantics).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    /// Multiplicative repetition penalty (llama.cpp / vLLM semantics).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f64>,
    /// Sampling seed for best-effort determinism.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Stop sequences.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    /// Token id (string) -> bias.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub logit_bias: BTreeMap<String, f64>,
    /// Thinking budget in tokens; `Some(0)` turns thinking off where allowed.
    /// Wins over [`ChatRequest::reasoning_effort`] on budget-style providers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
    /// Constrain output to a JSON object.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub json_mode: bool,
    /// Constrain output to this JSON Schema (implies JSON output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<Value>,
}

/// One model request. Everything the model sees is in here, so it can be
/// reconstructed from the session log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Provider-local model id.
    pub model: String,
    /// Prompt, system sections first.
    pub messages: Vec<Message>,
    /// Tools offered to the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    /// Tool-calling policy.
    #[serde(default)]
    pub tool_choice: ToolChoice,
    /// Sampling temperature; `None` = provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Output token cap; `None` = provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Reasoning budget; ignored by models without reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Prompt caching hint. `None` = adapter default. Only adapters with
    /// explicit cache breakpoints (Anthropic) act on it; prefix-cache
    /// providers (DeepSeek, OpenAI) cache automatically and ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<bool>,
    /// Remaining sampling / reasoning / output knobs.
    #[serde(default, flatten)]
    pub sampling: SamplingParams,
}

impl ChatRequest {
    /// Request with only a model and messages; everything else defaulted.
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::default(),
            temperature: None,
            max_output_tokens: None,
            reasoning_effort: None,
            prompt_cache: None,
            sampling: SamplingParams::default(),
        }
    }
}
