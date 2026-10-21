//! Normalised streaming vocabulary emitted by every adapter.

use serde::{Deserialize, Serialize};

use crate::usage::Usage;

/// Why generation stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Natural end of turn.
    Stop,
    /// Hit the output token limit.
    Length,
    /// Model requested tool calls.
    ToolCalls,
    /// Provider safety filter intervened.
    ContentFilter,
    /// Provider-specific reason not mapped above.
    Other,
}

/// One incremental piece of a model response.
///
/// Tool calls arrive as deltas keyed by `index`: the first delta for an index
/// carries `id` and `name`, later ones append to `arguments_delta`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Visible assistant text.
    TextDelta {
        /// Text fragment.
        text: String,
    },
    /// Reasoning/thinking text (`reasoning_content` and equivalents).
    ReasoningDelta {
        /// Reasoning fragment.
        text: String,
    },
    /// Partial tool call.
    ToolCallDelta {
        /// Position of the call within this response.
        index: u32,
        /// Call id, present on the first delta for this index.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Tool name, present on the first delta for this index.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Raw JSON argument fragment to append.
        arguments_delta: String,
    },
    /// Token usage; may arrive more than once, last value wins.
    Usage(Usage),
    /// Terminal event; no events follow.
    Finish(FinishReason),
}
