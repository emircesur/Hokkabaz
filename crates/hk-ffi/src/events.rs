//! Maps the runtime's JSON feed ([`hk_server::ServerEvent`]) to [`SessionEventDto`].

use hk_llm::{StreamEvent, Usage};
use hk_server::ServerEvent;
use hk_session::{EventKind, SessionEvent, TurnEndReason};
use serde_json::Value;

use crate::dto::{SessionEventDto, TurnEndReasonDto};

/// Bus chunks of the streaming model response (never logged).
const STREAM: &str = "agent/assistant-stream";
/// Agent loop failure the runtime reports outside the log.
const LOOP_ERROR: &str = "session/error";

pub fn usage_event(u: &Usage) -> SessionEventDto {
    SessionEventDto::Usage {
        input_tokens: to_i64(u.input_tokens),
        output_tokens: to_i64(u.output_tokens),
        cache_read_tokens: to_i64(u.cache_read_tokens),
        cache_write_tokens: to_i64(u.cache_write_tokens),
        reasoning_tokens: to_i64(u.reasoning_tokens),
    }
}

pub fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Zero, one or two DTOs per feed entry. Unknown or uninteresting entries map to nothing.
pub fn map(ev: ServerEvent) -> Vec<SessionEventDto> {
    match ev.name.as_str() {
        STREAM => map_stream(&ev.data),
        LOOP_ERROR => {
            let message = ev.data.get("message").and_then(Value::as_str).unwrap_or("agent loop failed");
            vec![SessionEventDto::Error { message: message.to_owned(), fatal: true }]
        }
        _ => match serde_json::from_value::<SessionEvent>(ev.data) {
            Ok(e) => map_logged(e.kind),
            Err(e) => {
                tracing::debug!(error = %e, name = %ev.name, "unmapped session event");
                Vec::new()
            }
        },
    }
}

fn map_stream(data: &Value) -> Vec<SessionEventDto> {
    let Some(Ok(ev)) = data.get("event").cloned().map(serde_json::from_value::<StreamEvent>) else {
        return Vec::new();
    };
    match ev {
        StreamEvent::TextDelta { text } => vec![SessionEventDto::TextDelta { text }],
        StreamEvent::ReasoningDelta { text } => vec![SessionEventDto::ReasoningDelta { text }],
        StreamEvent::ToolCallDelta { index, id, name, arguments_delta } => {
            let mut out = Vec::new();
            // First delta of a call carries id+name; later ones only append arguments.
            if let (Some(call_id), Some(name)) = (id, name) {
                out.push(SessionEventDto::ToolCallStart { index, call_id, name });
            }
            if !arguments_delta.is_empty() {
                out.push(SessionEventDto::ToolCallArgs { index, delta: arguments_delta });
            }
            out
        }
        // Usage and finish arrive durably via step/end.
        StreamEvent::Usage(_) | StreamEvent::Finish(_) => Vec::new(),
    }
}

fn map_logged(kind: EventKind) -> Vec<SessionEventDto> {
    let one = |e| vec![e];
    match kind {
        EventKind::TurnStart { turn_id } => one(SessionEventDto::TurnStarted { turn_id }),
        EventKind::TurnEnd { turn_id, reason } => {
            one(SessionEventDto::TurnEnded { turn_id, reason: end_reason(reason) })
        }
        EventKind::StepEnd { usage: Some(u), .. } => one(usage_event(&u)),
        EventKind::UserMessage { content } => one(SessionEventDto::UserMessage { content }),
        EventKind::AssistantMessage { content, reasoning } => {
            one(SessionEventDto::AssistantMessage { content, reasoning })
        }
        EventKind::AssistantAttempt { error: Some(message), .. } => {
            one(SessionEventDto::Error { message, fatal: false })
        }
        EventKind::ToolCall { call_id, name, arguments } => one(SessionEventDto::ToolCall {
            call_id,
            name,
            arguments_json: arguments.to_string(),
        }),
        EventKind::ToolResult { call_id, content, is_error } => {
            one(SessionEventDto::ToolResult { call_id, content, is_error })
        }
        EventKind::ApprovalRequest { approval_id, call_id, tool, detail } => {
            one(SessionEventDto::ApprovalRequest { approval_id, call_id, tool, detail })
        }
        EventKind::ApprovalResponse { approval_id, approved, reason } => {
            one(SessionEventDto::ApprovalResolved { approval_id, approved, reason })
        }
        EventKind::SessionTitle { title } => one(SessionEventDto::TitleChanged { title }),
        _ => Vec::new(),
    }
}

pub fn end_reason(r: TurnEndReason) -> TurnEndReasonDto {
    match r {
        TurnEndReason::Completed => TurnEndReasonDto::Completed,
        TurnEndReason::Cancelled => TurnEndReasonDto::Cancelled,
        TurnEndReason::Failed => TurnEndReasonDto::Failed,
        TurnEndReason::MaxSteps => TurnEndReasonDto::MaxSteps,
    }
}
