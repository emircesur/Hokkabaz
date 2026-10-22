//! hk-server: axum surface for a headless Hokkabaz instance.
//!
//! P4.6a ships the agent API (`/api/v1`: sessions, turns, approvals, models, WS event stream).
//! P4.6 adds the n8n-shaped surface (webhooks, REST v1 workflows / executions / credentials,
//! token scopes, rate limit, OpenAPI) once an [`AppState`] is given a [`FlowApi`]. Two feature tiers: `runtime` is the
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
#[cfg(feature = "concat")]
mod concat_tools;
mod error;
#[cfg(feature = "server")]
mod flow;
#[cfg(feature = "server")]
mod flow_tools;
mod local;
#[cfg(feature = "server")]
mod n8n;
#[cfg(feature = "server")]
mod openapi;
#[cfg(feature = "server")]
mod pairing;
#[cfg(feature = "server")]
mod payload;
#[cfg(feature = "server")]
mod providers;
#[cfg(feature = "server")]
mod ratelimit;
mod runtime;
#[cfg(feature = "server")]
mod signature;
#[cfg(feature = "server")]
mod studio;
#[cfg(all(test, feature = "server"))]
mod tests;
#[cfg(all(test, feature = "server"))]
mod tests_flow;
#[cfg(all(test, feature = "server"))]
mod tests_studio;
#[cfg(feature = "server")]
mod webhook;
#[cfg(feature = "server")]
mod ws;

#[cfg(feature = "server")]
pub use api::{AppState, router, serve, serve_listener};
#[cfg(feature = "server")]
pub use auth::{Auth, AuthError, Grants, Principal, Reject, Scope};
#[cfg(feature = "concat")]
pub use concat_tools::{ConcatConfig, ConcatSession, concat_tools};
pub use error::ApiError;
#[cfg(feature = "server")]
pub use flow::{BuiltinCredentialSchemas, CredentialSchemas, FlowApi, WorkflowActivator};
#[cfg(feature = "server")]
pub use flow_tools::{FlowToolHost, TestRun, flow_tools};
pub use local::{LocalConfig, LocalRuntime};
#[cfg(feature = "server")]
pub use n8n::workflow_json;
#[cfg(feature = "server")]
pub use openapi::ApiDoc;
#[cfg(feature = "server")]
pub use pairing::{Minted, Pairing, PairingError, qr_svg, qr_terminal};
#[cfg(feature = "server")]
pub use ratelimit::{RateLimit, RateLimiter, Throttled};
pub use runtime::{
    AgentRuntime, ApprovalDecision, CreateSession, ServerEvent, SessionDetail, SessionInfo,
    TurnAccepted, TurnMode, TurnRequest,
};
#[cfg(feature = "server")]
pub use signature::{HmacAlgorithm, HmacPreset, HmacSpec, SigEncoding, SignatureError};
#[cfg(feature = "server")]
pub use studio::{StudioApi, Tickets};
#[cfg(feature = "server")]
pub use webhook::{
    ChannelSink, LastNodeData, ResponseMode, WebhookAuth, WebhookCall, WebhookHandler, WebhookMode,
    WebhookRegistry, WebhookResult, WebhookRoute, WebhookRun, WebhookSpec, WebhookTarget,
};
