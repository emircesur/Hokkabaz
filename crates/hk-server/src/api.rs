//! `/api/v1` routes.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::auth::{Auth, Principal, Scope};
use crate::error::ApiError;
use crate::runtime::{
    AgentRuntime, ApprovalDecision, CreateSession, SessionDetail, SessionInfo, TurnAccepted,
    TurnRequest,
};
use crate::ws;

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;

/// Shared handler state. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<dyn AgentRuntime>,
    pub auth: Auth,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").field("auth", &self.auth).finish_non_exhaustive()
    }
}

impl AppState {
    pub fn new(runtime: Arc<dyn AgentRuntime>, auth: Auth) -> Self {
        Self { runtime, auth }
    }
}

/// The agent API router. Mount under a larger app or hand to [`serve`].
pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/health", get(health))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/turns", post(submit_turn))
        .route("/sessions/{id}/inject", post(inject))
        .route("/sessions/{id}/cancel", post(cancel))
        .route("/approvals/{id}", post(approve))
        .route("/models", get(models))
        .route("/ws", get(ws::upgrade));
    Router::new().nest("/api/v1", v1).with_state(state)
}

/// Bind `addr` and serve until the process ends.
pub async fn serve(addr: SocketAddr, state: AppState) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    serve_listener(listener, state, std::future::pending()).await
}

/// Serve on an already bound listener until `shutdown` resolves.
pub async fn serve_listener(
    listener: TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    tracing::info!(addr = ?listener.local_addr().ok(), "hk-server listening");
    axum::serve(listener, router(state)).with_graceful_shutdown(shutdown).await
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

#[derive(Deserialize)]
struct Page {
    limit: Option<u32>,
    offset: Option<u32>,
}

async fn list_sessions(
    p: Principal,
    State(s): State<AppState>,
    Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Read)?;
    let limit = page.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let sessions = s.runtime.list_sessions(limit, page.offset.unwrap_or(0)).await?;
    Ok(Json(json!({ "sessions": sessions })))
}

async fn create_session(
    p: Principal,
    State(s): State<AppState>,
    body: Option<Json<CreateSession>>,
) -> Result<(StatusCode, Json<SessionInfo>), ApiError> {
    p.require(Scope::Run)?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    Ok((StatusCode::CREATED, Json(s.runtime.create_session(req).await?)))
}

#[derive(Deserialize)]
struct Cursor {
    after_seq: Option<u64>,
}

async fn get_session(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(c): Query<Cursor>,
) -> Result<Json<SessionDetail>, ApiError> {
    p.require(Scope::Read)?;
    Ok(Json(s.runtime.session(&id, c.after_seq).await?))
}

async fn submit_turn(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<TurnRequest>,
) -> Result<(StatusCode, Json<TurnAccepted>), ApiError> {
    p.require(Scope::Run)?;
    if req.prompt.trim().is_empty() {
        return Err(ApiError::BadRequest("prompt must not be empty".into()));
    }
    Ok((StatusCode::ACCEPTED, Json(s.runtime.submit(&id, req).await?)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InjectBody {
    text: String,
}

async fn inject(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<InjectBody>,
) -> Result<StatusCode, ApiError> {
    p.require(Scope::Run)?;
    if b.text.trim().is_empty() {
        return Err(ApiError::BadRequest("text must not be empty".into()));
    }
    s.runtime.inject(&id, b.text).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn cancel(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    p.require(Scope::Run)?;
    s.runtime.cancel(&id).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn approve(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(d): Json<ApprovalDecision>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Run)?;
    s.runtime.respond_approval(&id, d).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn models(p: Principal, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Read)?;
    Ok(Json(json!({ "models": s.runtime.models().await? })))
}
