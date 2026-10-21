//! hk-agent: the turn/step agent loop.
//!
//! One turn = claim inbox -> N steps (`agent/pre-step`, `agent/request`, stream, durable
//! events, tool pipeline) until the model owes no more tool results, the step cap hits, or
//! the turn is cancelled. Modes (`build` / `plan`) gate tools through a `tools/pre-execute`
//! interceptor; `Ask` calls park on an `approval/request` until `AgentHandle::respond`. Every model request is `derive_messages(log)` plus a logged
//! `request/header`, so nothing the model sees is missing from the session log.
// port: dsh packages/core/agent-loop (turn/step flow, inbox claim, inject at step boundary)

mod agent;
mod approval;
mod config;
mod events;
mod inbox;
mod mode;
#[cfg(test)]
mod tests;

pub use agent::{Agent, AgentError, TurnSummary};
pub use config::AgentConfig;
pub use events::{AgentPreStep, AgentRequest, AgentStream, ApprovalRequest, ApprovalResponse};
pub use inbox::AgentHandle;
pub use mode::{Decision, Mode};
