//! Sessions, turns, approvals and the live event stream.

use crate::dto::{
    CreateSessionDto, ModeDto, SessionDetailDto, SessionEventDto, SessionInfoDto, TurnAcceptedDto,
    TurnRequestDto,
};
use crate::error::{HkError, HkErrorKind};
use crate::frb_generated::StreamSink;
use crate::handle::core;
use futures::StreamExt;

/// New empty session. `model` is `provider/model`; both it and the workspace default from
/// settings.
pub async fn create_session(
    title: Option<String>,
    model: Option<String>,
    workspace_root: Option<String>,
) -> Result<SessionInfoDto, HkError> {
    core()?
        .create_session(CreateSessionDto {
            title,
            model,
            workspace_root,
        })
        .await
}

/// Most recently updated first.
pub async fn list_sessions(limit: u32, offset: u32) -> Result<Vec<SessionInfoDto>, HkError> {
    core()?.list_sessions(limit, offset).await
}

/// Derived messages (what the model would see) plus approvals still waiting for an answer.
pub async fn get_session(session_id: String) -> Result<SessionDetailDto, HkError> {
    core()?.get_session(&session_id).await
}

/// Queue a user prompt; the turn runs in the background and reports through `session_events`.
/// `mode` `None` = settings default; `model` `None` keeps the session's model.
pub async fn submit_turn(
    session_id: String,
    prompt: String,
    mode: Option<ModeDto>,
    model: Option<String>,
) -> Result<TurnAcceptedDto, HkError> {
    core()?
        .submit_turn(
            &session_id,
            TurnRequestDto {
                prompt,
                mode,
                model,
            },
        )
        .await
}

/// Steer the running turn with an extra user message (applied at the next step boundary).
pub async fn inject(session_id: String, text: String) -> Result<(), HkError> {
    core()?.inject(&session_id, text).await
}

/// Abort the running turn (no-op when idle).
pub async fn cancel(session_id: String) -> Result<(), HkError> {
    core()?.cancel(&session_id).await
}

/// Answer an `ApprovalRequest` event.
pub async fn respond_approval(
    session_id: String,
    approval_id: String,
    approved: bool,
    reason: Option<String>,
) -> Result<(), HkError> {
    core()?
        .respond_approval(&session_id, &approval_id, approved, reason)
        .await
}

/// Live events of one session until `core_shutdown`. Listen *before* `submit_turn`; history is
/// `get_session`. Cancelling the Dart subscription is noticed on the next event of the session.
pub async fn session_events(
    session_id: String,
    sink: StreamSink<SessionEventDto>,
) -> Result<(), HkError> {
    let mut feed = core()?.session_events(&session_id)?;
    while let Some(ev) = feed.next().await {
        sink.add(ev)
            .map_err(|e| HkError::new(HkErrorKind::Closed, e.to_string()))?;
    }
    Ok(())
}
