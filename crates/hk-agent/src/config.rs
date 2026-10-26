use std::path::PathBuf;
use std::time::Duration;

use hk_llm::{ModelCost, ReasoningEffort, ToolChoice};

use crate::mode::Mode;

/// Loop guards. Everything is off or lenient by default; hosts tighten per agent.
#[derive(Debug, Clone)]
pub struct Guards {
    /// Max time for one model request (start of request until the stream finishes).
    pub step_timeout: Option<Duration>,
    /// Max time for one tool call. Time parked on an approval does not count, and `task`
    /// (subagents) is exempt: the child's own guards bound it.
    pub tool_timeout: Option<Duration>,
    /// Max wall time for a whole turn, approvals included.
    pub turn_timeout: Option<Duration>,
    /// Stop the turn once its priced usage exceeds this many USD. Needs `price`.
    pub max_cost_usd: Option<f64>,
    /// Per-million-token prices of `AgentConfig::model` (from the catalog).
    pub price: Option<ModelCost>,
    /// Identical tool call (name + args) this many times in a row adds a reminder. 0 = off.
    pub repeat_remind_after: u32,
    /// ...and ends the turn as failed at this many. 0 = never abort.
    pub repeat_abort_after: u32,
}

impl Default for Guards {
    fn default() -> Self {
        Self {
            step_timeout: None,
            tool_timeout: None,
            turn_timeout: None,
            max_cost_usd: None,
            price: None,
            repeat_remind_after: 3,
            repeat_abort_after: 6,
        }
    }
}

/// When and how history is summarized.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Compact before a step when the context is estimated at or above this many tokens.
    pub threshold_tokens: u64,
    /// Steps at the tail of the log that stay verbatim.
    pub keep_recent_steps: usize,
}

impl CompactionConfig {
    /// Threshold with the default 4-step verbatim tail.
    pub fn new(threshold_tokens: u64) -> Self {
        Self {
            threshold_tokens,
            keep_recent_steps: 4,
        }
    }
}

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
    /// Repeat-tool, timeout and cost guards.
    pub guards: Guards,
    /// Automatic summarization of old history; `None` = never compact on its own.
    pub compaction: Option<CompactionConfig>,
    /// Remaining sampling / reasoning / output knobs (layered AI settings).
    pub sampling: hk_llm::SamplingParams,
    /// Approval overrides keyed `tool:mode` (`build`/`plan`) → `allow` | `ask` | `deny`.
    pub policy: std::collections::BTreeMap<String, String>,
    /// Dry run: tools that are not read-only report what they would do instead of running.
    pub dry_run: bool,
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
            guards: Guards::default(),
            compaction: None,
            sampling: hk_llm::SamplingParams::default(),
            policy: std::collections::BTreeMap::new(),
            dry_run: false,
        }
    }
}
