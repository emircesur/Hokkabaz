//! Service boundary between the HTTP/WS layer and the agent machinery.
//!
//! Handlers only know this trait, so they test against a fake and the daemon can swap in other
//! backings (remote, multi-tenant) without touching routes.

use hk_llm::{Message, ModelInfo};
use hk_session::SessionEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::error::ApiError;

/// Session list row / header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: Option<String>,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub event_count: u64,
    pub turn_count: u64,
    /// A turn is executing right now.
    pub running: bool,
}

/// Body of `POST /sessions`; everything optional.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSession {
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub title: Option<String>,
}

/// Tool policy for a turn (mirrors `hk_agent::Mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnMode {
    Build,
    Plan,
}

/// Body of `POST /sessions/{id}/turns`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnRequest {
    pub prompt: String,
    /// Switch mode from this turn on; omitted keeps the current one.
    #[serde(default)]
    pub mode: Option<TurnMode>,
    /// Switch model from this turn on (provider-local id or `provider/model`).
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnAccepted {
    pub session_id: String,
    /// Another turn was already running; the prompt is queued behind it.
    pub queued: bool,
}

/// Body of `POST /approvals/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalDecision {
    pub approved: bool,
    #[serde(default)]
    pub reason: Option<String>,
    /// Approval ids are per session (`appr_0` repeats); required when more than one session
    /// has this id pending.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// `GET /sessions/{id}`: log tail plus the history the model would see.
#[derive(Debug, Clone, Serialize)]
pub struct SessionDetail {
    pub session: SessionInfo,
    /// Logged events with `seq > after_seq` (all when no cursor).
    pub events: Vec<SessionEvent>,
    /// `derive_messages` over the whole log.
    pub messages: Vec<Message>,
    /// Approvals parked right now, for UIs attaching late.
    pub pending_approvals: Vec<String>,
}

/// One pushed event. `name` is `domain/action`; `data` fields are snake_case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerEvent {
    #[serde(rename = "event")]
    pub name: String,
    pub session_id: String,
    pub data: Value,
}

/// What the API needs from the agent side.
#[async_trait::async_trait]
pub trait AgentRuntime: Send + Sync + 'static {
    async fn list_sessions(&self, limit: u32, offset: u32) -> Result<Vec<SessionInfo>, ApiError>;
    async fn create_session(&self, req: CreateSession) -> Result<SessionInfo, ApiError>;
    async fn session(&self, id: &str, after_seq: Option<u64>) -> Result<SessionDetail, ApiError>;
    async fn submit(&self, id: &str, req: TurnRequest) -> Result<TurnAccepted, ApiError>;
    async fn inject(&self, id: &str, text: String) -> Result<(), ApiError>;
    async fn cancel(&self, id: &str) -> Result<(), ApiError>;
    /// `NotFound` when nothing pending matches (settled, cancelled, wrong id).
    async fn respond_approval(&self, approval_id: &str, decision: ApprovalDecision) -> Result<(), ApiError>;
    async fn models(&self) -> Result<Vec<ModelInfo>, ApiError>;
    /// Live feed: agent bus events and logged session events for every session.
    fn subscribe(&self) -> broadcast::Receiver<ServerEvent>;
    /// Extra tools offered to sessions started after this call (flow tools etc.). Backings
    /// without a tool registry ignore it.
    fn add_tools(&self, _tools: Vec<std::sync::Arc<dyn hk_tools::Tool>>) {}

    /// Headless Studio shared with desktop and remote controllers.
    fn studio(&self) -> Option<std::sync::Arc<hk_studio::Studio>> { None }
}
