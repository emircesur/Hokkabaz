//! hk-agent: the turn/step agent loop.
//!
//! One turn = claim inbox -> N steps (`agent/pre-step`, `agent/request`, stream, durable
//! events, tool pipeline) until the model owes no more tool results, the step cap hits, or
//! the turn is cancelled. Modes (`build` / `plan`) gate tools through a `tools/pre-execute`
//! interceptor; `Ask` calls park on an `approval/request` until `AgentHandle::respond`. Every
//! model request is `derive_messages(log)` plus a logged `request/header`, so nothing the model
//! sees is missing from the session log. Guards (repeat-tool, timeouts, cost cap) end or nudge
//! turns, compaction swaps old history for a logged summary, and the `task` tool runs
//! subagents in linked child sessions.
// port: dsh packages/core/agent-loop (turn/step flow, inbox claim, inject at step boundary)

mod agent;
mod approval;
mod compaction;
mod config;
mod events;
mod guard;
mod inbox;
mod mode;
mod subagent;
#[cfg(test)]
mod tests;

pub use agent::{Agent, AgentError, TurnSummary};
pub use config::{AgentConfig, CompactionConfig, Guards};
pub use events::{AgentPreStep, AgentRequest, AgentStream, ApprovalRequest, ApprovalResponse};
pub use inbox::AgentHandle;
pub use mode::{Decision, Mode};
pub use subagent::{ChildApprovals, SubagentRunner, TaskTool};
