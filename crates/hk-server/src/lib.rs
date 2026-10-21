//! hk-server: axum surface for a headless Hokkabaz instance.
//!
//! P4.6a ships the agent API (`/api/v1`: sessions, turns, approvals, models, WS event stream).
//! Webhook and n8n-shaped routes land with P4.6 proper. Everything sits behind the `server`
//! feature so targets that never serve (android, ffi) don't compile axum.
//!
//! The HTTP layer only talks to the [`AgentRuntime`] trait; [`LocalRuntime`] backs it with real
//! `hk_agent::Agent`s, tests use a fake.
// port: none (Hokkabaz-native; see DESIGN §3.9)
#![cfg(feature = "server")]

mod api;
mod auth;
mod error;
mod local;
mod runtime;
#[cfg(test)]
mod tests;
mod ws;

pub use api::{AppState, router, serve, serve_listener};
pub use auth::{Auth, AuthError, Scope};
pub use error::ApiError;
pub use local::{LocalConfig, LocalRuntime};
pub use runtime::{
    AgentRuntime, ApprovalDecision, CreateSession, SessionDetail, SessionInfo, ServerEvent,
    TurnAccepted, TurnMode, TurnRequest,
};
