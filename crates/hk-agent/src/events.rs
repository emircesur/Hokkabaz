//! Bus events the loop emits. `agent` is an interceptable domain, so plugins/guards can hook in.

use hk_core::Event;
use hk_llm::{ChatRequest, StreamEvent};
use hk_tools::Risk;

/// Before each step's request. Interceptors push `reminders`; the loop logs them as user
/// messages (model-visible means logged). Rejecting ends the turn as failed (guards, caps).
#[derive(Debug, Clone)]
pub struct AgentPreStep {
    /// Owning turn.
    pub turn_id: String,
    /// 0-based step.
    pub step: u32,
    /// Extra user-role text to log before this step's request.
    pub reminders: Vec<String>,
}
impl Event for AgentPreStep {
    const NAME: &'static str = "agent/pre-step";
}

/// The assembled request, just before it is sent. Interceptors may rewrite everything except
/// `messages`: those are derived from the log and any rewrite of them is discarded.
#[derive(Debug, Clone)]
pub struct AgentRequest {
    /// Owning turn.
    pub turn_id: String,
    /// 0-based step.
    pub step: u32,
    /// Request about to be sent.
    pub request: ChatRequest,
}
impl Event for AgentRequest {
    const NAME: &'static str = "agent/request";
}

/// Live-only stream chunk for UIs; never written to the session log.
#[derive(Debug, Clone)]
pub struct AgentStream {
    /// Owning turn.
    pub turn_id: String,
    /// 0-based step.
    pub step: u32,
    /// Normalised provider event.
    pub event: StreamEvent,
}
impl Event for AgentStream {
    const NAME: &'static str = "agent/assistant-stream";
}

/// A tool call is parked until someone answers via `AgentHandle::respond`. Emitted after the
/// matching `approval/request` session event is logged, so the id is safe to answer.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// Id to pass to `AgentHandle::respond`.
    pub approval_id: String,
    /// Call waiting on the decision.
    pub call_id: String,
    /// Tool name.
    pub tool: String,
    /// Risk the tool reported for these args.
    pub risk: Risk,
    /// Command / path / url summary for the approval card.
    pub detail: Option<String>,
}
impl Event for ApprovalRequest {
    const NAME: &'static str = "approval/request";
}

/// Decision for an [`ApprovalRequest`]; emitted after it is logged.
#[derive(Debug, Clone)]
pub struct ApprovalResponse {
    /// Approval this settles.
    pub approval_id: String,
    /// Granted or denied (cancelled turns count as denied).
    pub approved: bool,
    /// Approver's note.
    pub reason: Option<String>,
}
impl Event for ApprovalResponse {
    const NAME: &'static str = "approval/response";
}
