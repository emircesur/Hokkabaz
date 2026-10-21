use std::path::PathBuf;

use hk_llm::{ReasoningEffort, ToolChoice};

use crate::mode::Mode;

/// Static per-agent settings. Mode/model switching (P1.10) swaps these between turns.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Provider-local model id.
    pub model: String,
    /// System prompt sections; logged as `system/message` once, when the log has none yet.
    pub system: Vec<String>,
    /// Steps (model requests) per turn before the turn ends with `max_steps`.
    pub max_steps: u32,
    /// Tool-calling policy sent with every request.
    pub tool_choice: ToolChoice,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Output token cap.
    pub max_output_tokens: Option<u64>,
    /// Reasoning budget.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Working directory handed to tools.
    pub cwd: Option<PathBuf>,
    /// Tool policy: which tools are offered and which calls need approval.
    pub mode: Mode,
}

impl AgentConfig {
    /// Config with defaults: no system prompt, 50 steps, auto tool choice.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: Vec::new(),
            max_steps: 50,
            tool_choice: ToolChoice::Auto,
            temperature: None,
            max_output_tokens: None,
            reasoning_effort: None,
            cwd: None,
            mode: Mode::Build,
        }
    }
}
