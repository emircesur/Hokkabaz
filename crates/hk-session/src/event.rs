//! Durable session events. One JSON object per log line, named `domain/action`.
//!
//! Live-only traffic (`agent/assistant-stream` chunks) is deliberately absent:
//! it never reaches the log, only the finished `assistant/message` does.

use hk_llm::{FinishReason, ReasoningEffort, ToolChoice, ToolSpec, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One log line: monotonically increasing `seq` plus the payload.
///
/// `seq` is unique and increasing within a log but not necessarily contiguous:
/// a fork keeps the copied prefix's original `seq` values so that references
/// such as [`EventKind::CompactionSummary::through_seq`] stay valid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    /// Position in the log; `session/start` is always 0.
    pub seq: u64,
    /// Wall-clock time, milliseconds since the Unix epoch.
    pub ts_ms: i64,
    /// Event payload.
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Where a forked session was cut from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentRef {
    /// Source session.
    pub session_id: String,
    /// Last source `seq` copied into the fork.
    pub seq: u64,
}

/// Why a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEndReason {
    /// Model finished without owing more tool results.
    Completed,
    /// User or parent cancelled.
    Cancelled,
    /// Unrecoverable error.
    Failed,
    /// Step cap reached.
    MaxSteps,
}

/// What a model request carried besides the messages (which derive from the
/// log). Together with [`crate::derive_messages`] this reconstructs the request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestHeader {
    /// Provider-local model id.
    pub model: String,
    /// Tool schemas offered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSpec>,
    /// Tool-calling policy.
    #[serde(default)]
    pub tool_choice: ToolChoice,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Output cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Reasoning budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Number of derived messages sent; a cheap replay sanity check.
    pub message_count: u32,
}

/// Event payloads. The `type` tag is the `domain/action` name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EventKind {
    /// First line of every log; carries session metadata.
    #[serde(rename = "session/start")]
    SessionStart {
        /// Session id (matches the directory name).
        session_id: String,
        /// Working directory the session runs in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Initial model id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Initial title.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// Set when this session is a fork.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<ParentRef>,
    },
    /// Title changed (user rename or auto-title).
    #[serde(rename = "session/title")]
    SessionTitle {
        /// New title.
        title: String,
    },
    /// Turn opened; user messages for it are logged after this.
    #[serde(rename = "turn/start")]
    TurnStart {
        /// Turn id.
        turn_id: String,
    },
    /// Turn closed. Log is fsynced here.
    #[serde(rename = "turn/end")]
    TurnEnd {
        /// Turn id.
        turn_id: String,
        /// Why it closed.
        reason: TurnEndReason,
    },
    /// One model request/response cycle began.
    #[serde(rename = "step/start")]
    StepStart {
        /// Owning turn.
        turn_id: String,
        /// 0-based step within the turn.
        step: u32,
    },
    /// Step finished.
    #[serde(rename = "step/end")]
    StepEnd {
        /// Owning turn.
        turn_id: String,
        /// Step number.
        step: u32,
        /// Provider finish reason, if one arrived.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish: Option<FinishReason>,
        /// Usage for this step's request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
    /// System prompt section.
    #[serde(rename = "system/message")]
    SystemMessage {
        /// Text.
        content: String,
    },
    /// User input.
    #[serde(rename = "user/message")]
    UserMessage {
        /// Text.
        content: String,
    },
    /// Finished assistant text. Its tool calls follow as `tool/call` events.
    #[serde(rename = "assistant/message")]
    AssistantMessage {
        /// Visible text.
        content: String,
        /// Reasoning text, kept so providers that need it can replay it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
    },
    /// A request attempt (retry bookkeeping). Not part of model history.
    #[serde(rename = "assistant/attempt")]
    AssistantAttempt {
        /// Step the attempt belongs to.
        step: u32,
        /// 1-based attempt number.
        attempt: u32,
        /// Error that ended the attempt, if it failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Tool call requested by the model; attaches to the latest assistant message.
    #[serde(rename = "tool/call")]
    ToolCall {
        /// Provider call id.
        call_id: String,
        /// Tool name.
        name: String,
        /// Parsed arguments.
        arguments: Value,
    },
    /// Result returned to the model for a call.
    #[serde(rename = "tool/result")]
    ToolResult {
        /// Call this answers.
        call_id: String,
        /// Output text as the model sees it (after truncation/spill).
        content: String,
        /// Whether the tool failed; UI hint only.
        #[serde(default)]
        is_error: bool,
    },
    /// Request metadata, logged before each model request.
    #[serde(rename = "request/header")]
    RequestHeader(RequestHeader),
    /// Older history replaced by a summary.
    #[serde(rename = "compaction/summary")]
    CompactionSummary {
        /// Summary text, shown to the model as a user message.
        summary: String,
        /// Non-system messages from events with `seq <= through_seq` are dropped.
        through_seq: u64,
    },
    /// Tool call needs a human decision.
    #[serde(rename = "approval/request")]
    ApprovalRequest {
        /// Approval id.
        approval_id: String,
        /// Pending call.
        call_id: String,
        /// Tool name.
        tool: String,
        /// Human-readable detail (command, path).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Decision for an approval.
    #[serde(rename = "approval/response")]
    ApprovalResponse {
        /// Approval id.
        approval_id: String,
        /// Granted or denied.
        approved: bool,
        /// Optional note from the approver.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}
