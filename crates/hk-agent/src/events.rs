//! Bus events the loop emits. `agent` is an interceptable domain, so plugins/guards can hook in.

use hk_core::Event;
use hk_llm::{ChatRequest, StreamEvent};

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
