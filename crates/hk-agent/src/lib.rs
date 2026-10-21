//! hk-agent: the turn/step agent loop.
//!
//! One turn = claim inbox -> N steps (`agent/pre-step`, `agent/request`, stream, durable
//! events, tool pipeline) until the model owes no more tool results, the step cap hits, or
//! the turn is cancelled. Every model request is `derive_messages(log)` plus a logged
//! `request/header`, so nothing the model sees is missing from the session log.
// port: dsh packages/core/agent-loop (turn/step flow, inbox claim, inject at step boundary)

mod agent;
mod config;
mod events;
mod inbox;
#[cfg(test)]
mod tests;

pub use agent::{Agent, AgentError, TurnSummary};
pub use config::AgentConfig;
pub use events::{AgentPreStep, AgentRequest, AgentStream};
pub use inbox::AgentHandle;
