//! hk-server: axum surface for a headless Hokkabaz instance.
//!
//! P4.6a ships the agent API (`/api/v1`: sessions, turns, approvals, models, WS event stream).
//! Webhook and n8n-shaped routes land with P4.6 proper. Two feature tiers: `runtime` is the
//! in-process agent runtime ([`LocalRuntime`] behind [`AgentRuntime`]) with no HTTP stack, so
//! targets that never serve (android, hk-ffi) can embed it; `server` adds axum on top.
//!
//! The HTTP layer only talks to the [`AgentRuntime`] trait; [`LocalRuntime`] backs it with real
//! `hk_agent::Agent`s, tests use a fake.
// port: none (Hokkabaz-native; see DESIGN §3.9)
#![cfg(feature = "runtime")]

#[cfg(feature = "server")]
mod api;
#[cfg(feature = "server")]
mod auth;
mod error;
mod local;
mod runtime;
#[cfg(all(test, feature = "server"))]
mod tests;
#[cfg(feature = "server")]
mod ws;

#[cfg(feature = "server")]
pub use api::{AppState, router, serve, serve_listener};
#[cfg(feature = "server")]
pub use auth::{Auth, AuthError, Scope};
pub use error::ApiError;
pub use local::{LocalConfig, LocalRuntime};
pub use runtime::{
    AgentRuntime, ApprovalDecision, CreateSession, SessionDetail, SessionInfo, ServerEvent,
    TurnAccepted, TurnMode, TurnRequest,
};
