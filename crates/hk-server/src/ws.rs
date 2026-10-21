//! `GET /api/v1/ws`: one socket per client. Server pushes [`ServerEvent`] frames
//! (`{"event":"tool/call","session_id":"..","data":{..}}`); the client sends JSON commands.
//!
//! Commands: `submit`, `inject`, `cancel`, `approve`, plus `subscribe` / `unsubscribe` to
//! narrow the feed to chosen sessions (default: all). Each command may carry an `id` that the
//! `command/ack` or `command/error` reply echoes. Mutating commands need the `run` scope.

use std::collections::HashSet;

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::api::AppState;
use crate::auth::{Scope, bearer};
use crate::error::ApiError;
use crate::runtime::{ApprovalDecision, ServerEvent, TurnMode, TurnRequest};

#[derive(Deserialize)]
pub(crate) struct WsQuery {
    /// Fallback for clients that can't set headers (browsers). Tokens in URLs end up in
    /// proxy logs; prefer the header.
    access_token: Option<String>,
}

pub(crate) async fn upgrade(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WsQuery>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    let token = bearer(&headers).or(q.access_token.as_deref()).ok_or(ApiError::Unauthorized)?;
    let scope = s.auth.authenticate(token).ok_or(ApiError::Unauthorized)?;
    if scope < Scope::Read {
        return Err(ApiError::Forbidden("read"));
    }
    // Upgrade errors only after auth so unauthenticated probes learn nothing about the route.
    let ws = ws.map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok(ws.on_upgrade(move |sock| session(sock, s, scope)))
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Command {
    Submit {
        session_id: String,
        prompt: String,
        #[serde(default)]
        mode: Option<TurnMode>,
        #[serde(default)]
        model: Option<String>,
    },
    Inject {
        session_id: String,
        text: String,
    },
    Cancel {
        session_id: String,
    },
    Approve {
        approval_id: String,
        approved: bool,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
    },
    Subscribe {
        session_id: String,
    },
    Unsubscribe {
        session_id: String,
    },
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    id: Option<Value>,
    #[serde(flatten)]
    cmd: Command,
}

async fn session(mut sock: WebSocket, s: AppState, scope: Scope) {
    let mut rx = s.runtime.subscribe();
    // Empty = everything.
    let mut filter: HashSet<String> = HashSet::new();
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    if (filter.is_empty() || filter.contains(&ev.session_id))
                        && send(&mut sock, &ev).await.is_err()
                    {
                        return;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    let lag = ServerEvent {
                        name: "stream/lagged".into(),
                        session_id: String::new(),
                        data: json!({ "skipped": n }),
                    };
                    if send(&mut sock, &lag).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Closed) => return,
            },
            msg = sock.recv() => match msg {
                Some(Ok(Message::Text(t))) => {
                    let Outcome(reply) = handle(&s, scope, &mut filter, t.as_str()).await;
                    if sock.send(Message::text(reply.to_string())).await.is_err() {
                        return;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                // Ping/pong answered by axum; binary frames are not part of the protocol.
                Some(Ok(_)) => {}
            },
        }
    }
}

async fn send(sock: &mut WebSocket, ev: &ServerEvent) -> Result<(), axum::Error> {
    let text = serde_json::to_string(ev).unwrap_or_else(|_| "{}".into());
    sock.send(Message::text(text)).await
}

/// Reply frame for one client command.
pub(crate) struct Outcome(pub(crate) Value);

pub(crate) async fn handle(s: &AppState, scope: Scope, filter: &mut HashSet<String>, text: &str) -> Outcome {
    let env: Envelope = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(e) => return Outcome(error_frame(None, &ApiError::BadRequest(format!("bad command: {e}")))),
    };
    let id = env.id;
    Outcome(match run(s, scope, filter, env.cmd).await {
        Ok(()) => json!({ "event": "command/ack", "id": id }),
        Err(e) => error_frame(id, &e),
    })
}

fn error_frame(id: Option<Value>, e: &ApiError) -> Value {
    json!({ "event": "command/error", "id": id, "data": { "code": e.code(), "message": e.to_string() } })
}

async fn run(s: &AppState, scope: Scope, filter: &mut HashSet<String>, cmd: Command) -> Result<(), ApiError> {
    let need_run = || {
        if scope >= Scope::Run { Ok(()) } else { Err(ApiError::Forbidden("run")) }
    };
    match cmd {
        Command::Subscribe { session_id } => {
            filter.insert(session_id);
            Ok(())
        }
        Command::Unsubscribe { session_id } => {
            filter.remove(&session_id);
            Ok(())
        }
        Command::Submit { session_id, prompt, mode, model } => {
            need_run()?;
            if prompt.trim().is_empty() {
                return Err(ApiError::BadRequest("prompt must not be empty".into()));
            }
            s.runtime.submit(&session_id, TurnRequest { prompt, mode, model }).await.map(drop)
        }
        Command::Inject { session_id, text } => {
            need_run()?;
            s.runtime.inject(&session_id, text).await
        }
        Command::Cancel { session_id } => {
            need_run()?;
            s.runtime.cancel(&session_id).await
        }
        Command::Approve { approval_id, approved, reason, session_id } => {
            need_run()?;
            s.runtime
                .respond_approval(&approval_id, ApprovalDecision { approved, reason, session_id })
                .await
        }
    }
}
