//! `/api/v1` routes.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::auth::{Auth, Principal, Scope};
use crate::error::ApiError;
use crate::flow::FlowApi;
use crate::pairing::{self, Pairing};
use crate::providers;
use crate::ratelimit::{RateLimit, RateLimiter};
use crate::runtime::{
    AgentRuntime, ApprovalDecision, CreateSession, SessionDetail, SessionInfo, TurnAccepted,
    TurnRequest,
};
use crate::studio::{self, StudioApi, Tickets};
use crate::{n8n, openapi, webhook, ws};

const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;

/// Shared handler state. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<dyn AgentRuntime>,
    pub auth: Auth,
    /// n8n-shaped surface (webhooks + REST v1). `None` leaves those routes unmounted.
    pub flow: Option<FlowApi>,
    /// Per-token limiter, charged on every authenticated request.
    pub limiter: Arc<RateLimiter>,
    /// Device pairing (`/api/v1/pairing`). `None` leaves those routes unmounted.
    pub pairing: Option<Arc<Pairing>>,
    /// Video Studio of this instance (`/api/v1/studio`). `None` leaves those routes unmounted.
    pub studio: Option<Arc<dyn StudioApi>>,
    pub tickets: Arc<Tickets>,
    /// Provider settings of this instance (`/api/v1/providers`, `/settings`). `None` leaves them unmounted.
    pub providers: Option<Arc<hk_providers::ProviderHub>>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl AppState {
    pub fn new(runtime: Arc<dyn AgentRuntime>, auth: Auth) -> Self {
        Self {
            runtime,
            auth,
            flow: None,
            limiter: Arc::new(RateLimiter::default()),
            pairing: None,
            studio: None,
            tickets: Arc::new(Tickets::default()),
            providers: None,
        }
    }

    /// Enable `/webhook`, `/webhook-test` and the n8n REST routes.
    #[must_use]
    pub fn with_flow(mut self, flow: FlowApi) -> Self {
        self.flow = Some(flow);
        self
    }

    /// Enable `GET /api/v1/pairing` and `POST /api/v1/pairing/exchange`.
    #[must_use]
    pub fn with_pairing(mut self, p: Pairing) -> Self {
        self.pairing = Some(Arc::new(p));
        self
    }

    /// Serve the Video Studio to remote apps.
    #[must_use]
    pub fn with_studio(mut self, studio: Arc<dyn StudioApi>) -> Self {
        self.studio = Some(studio);
        self
    }

    /// Let remote apps manage this instance's model providers.
    #[must_use]
    pub fn with_providers(mut self, hub: Arc<hk_providers::ProviderHub>) -> Self {
        self.providers = Some(hub);
        self
    }

    /// Replace the default token bucket (`None` disables limiting).
    #[must_use]
    pub fn with_rate_limit(mut self, cfg: Option<RateLimit>) -> Self {
        self.limiter = Arc::new(RateLimiter::new(cfg));
        self
    }
}

/// The agent API router. Mount under a larger app or hand to [`serve`].
pub fn router(state: AppState) -> Router {
    let mut v1 = Router::new()
        .route("/health", get(health))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/turns", post(submit_turn))
        .route("/sessions/{id}/inject", post(inject))
        .route("/sessions/{id}/cancel", post(cancel))
        .route("/approvals/{id}", post(approve))
        .route("/models", get(models))
        .route("/ws", get(ws::upgrade))
        .route("/openapi.json", get(openapi::openapi_json));
    let mut app = Router::new();
    if state.flow.is_some() {
        v1 = v1
            .route(
                "/workflows",
                get(n8n::list_workflows).post(n8n::create_workflow),
            )
            .route(
                "/workflows/{id}",
                get(n8n::get_workflow)
                    .put(n8n::update_workflow)
                    .delete(n8n::delete_workflow),
            )
            .route("/workflows/{id}/activate", post(n8n::activate_workflow))
            .route("/workflows/{id}/deactivate", post(n8n::deactivate_workflow))
            .route("/workflows/{id}/run", post(n8n::run_workflow))
            .route("/executions", get(n8n::list_executions))
            .route(
                "/executions/{id}",
                get(n8n::get_execution).delete(n8n::delete_execution),
            )
            .route("/credentials", post(n8n::create_credential))
            .route(
                "/credentials/{id}",
                axum::routing::delete(n8n::delete_credential),
            )
            .route(
                "/credentials/schema/{type_name}",
                get(n8n::credential_schema),
            );
        app = app
            .route("/webhook/{*path}", any(webhook::production))
            .route("/webhook-test/{*path}", any(webhook::test));
    }
    if state.pairing.is_some() {
        v1 = v1
            .route("/pairing", get(pairing::issue))
            .route("/pairing/exchange", post(pairing::exchange));
    }
    if state.providers.is_some() {
        v1 = v1
            .route("/settings", get(providers::get_settings))
            .route(
                "/settings/preferences",
                axum::routing::put(providers::set_preferences),
            )
            .route("/providers", axum::routing::put(providers::save_provider))
            .route(
                "/providers/{id}",
                axum::routing::delete(providers::remove_provider),
            )
            .route(
                "/providers/{id}/key",
                axum::routing::put(providers::set_key),
            )
            .route("/providers/models", get(providers::models))
            .route("/providers/catalog", post(providers::refresh_catalog));
    }
    if state.studio.is_some() {
        v1 = v1
            .route("/studio/rpc/{method}", post(studio::rpc))
            .route("/studio/events/{id}", get(studio::events))
            .route(
                "/studio/upload",
                axum::routing::put(studio::upload)
                    .layer(axum::extract::DefaultBodyLimit::disable()),
            )
            .route("/studio/ticket", post(studio::ticket))
            .route("/studio/media", get(studio::media));
    }
    app.nest("/api/v1", v1).with_state(state)
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
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
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
    let sessions = s
        .runtime
        .list_sessions(limit, page.offset.unwrap_or(0))
        .await?;
    Ok(Json(json!({ "sessions": sessions })))
}

async fn create_session(
    p: Principal,
    State(s): State<AppState>,
    body: Option<Json<CreateSession>>,
) -> Result<(StatusCode, Json<SessionInfo>), ApiError> {
    p.require(Scope::Run)?;
    let mut req = body.map(|Json(b)| b).unwrap_or_default();
    // The app picks the default model in the instance settings; the runtime's own default is only a floor.
    if req.model.is_none()
        && let Some(hub) = &s.providers
    {
        req.model = hub.default_model().await;
    }
    Ok((
        StatusCode::CREATED,
        Json(s.runtime.create_session(req).await?),
    ))
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
    Ok((
        StatusCode::ACCEPTED,
        Json(s.runtime.submit(&id, req).await?),
    ))
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
