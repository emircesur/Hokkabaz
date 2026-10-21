//! Maps the runtime's JSON feed ([`hk_server::ServerEvent`]) to [`SessionEventDto`].

use hk_llm::StreamEvent;
use hk_server::ServerEvent;
use hk_session::{EventKind, SessionEvent, TurnEndReason};
use serde_json::Value;

use crate::dto::{SessionEventDto as Ev, SessionEventKind as K, TurnEndReasonDto, UsageDto};

/// Bus chunks of the streaming model response (never logged).
const STREAM: &str = "agent/assistant-stream";
/// Agent loop failure the runtime reports outside the log.
const LOOP_ERROR: &str = "session/error";

pub fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn text_ev(kind: K, text: String) -> Ev {
    Ev { text: Some(text), ..Ev::new(kind) }
}

/// Zero, one or two DTOs per feed entry. Unknown or uninteresting entries map to nothing.
pub fn map(ev: ServerEvent) -> Vec<Ev> {
    match ev.name.as_str() {
        STREAM => map_stream(&ev.data),
        LOOP_ERROR => {
            let message = ev.data.get("message").and_then(Value::as_str).unwrap_or("agent loop failed");
            vec![Ev { fatal: true, ..text_ev(K::Error, message.to_owned()) }]
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

fn map_stream(data: &Value) -> Vec<Ev> {
    let Some(Ok(ev)) = data.get("event").cloned().map(serde_json::from_value::<StreamEvent>) else {
        return Vec::new();
    };
    match ev {
        StreamEvent::TextDelta { text } => vec![text_ev(K::TextDelta, text)],
        StreamEvent::ReasoningDelta { text } => vec![text_ev(K::ReasoningDelta, text)],
        StreamEvent::ToolCallDelta { index, id, name, arguments_delta } => {
            let mut out = Vec::new();
            // First delta of a call carries id+name; later ones only append arguments.
            if let (Some(call_id), Some(name)) = (id, name) {
                out.push(Ev { index: Some(index), call_id: Some(call_id), name: Some(name), ..Ev::new(K::ToolCallStart) });
            }
            if !arguments_delta.is_empty() {
                out.push(Ev { index: Some(index), ..text_ev(K::ToolCallArgs, arguments_delta) });
            }
            out
        }
        // Usage and finish arrive durably via step/end.
        StreamEvent::Usage(_) | StreamEvent::Finish(_) => Vec::new(),
    }
}

fn map_logged(kind: EventKind) -> Vec<Ev> {
    let one = |e| vec![e];
    match kind {
        EventKind::TurnStart { turn_id } => one(Ev { turn_id: Some(turn_id), ..Ev::new(K::TurnStarted) }),
        EventKind::TurnEnd { turn_id, reason } => one(Ev {
            turn_id: Some(turn_id),
            end_reason: Some(end_reason(reason)),
            ..Ev::new(K::TurnEnded)
        }),
        EventKind::StepEnd { usage: Some(u), .. } => one(Ev {
            usage: Some(UsageDto {
                input_tokens: to_i64(u.input_tokens),
                output_tokens: to_i64(u.output_tokens),
                cache_read_tokens: to_i64(u.cache_read_tokens),
                cache_write_tokens: to_i64(u.cache_write_tokens),
                reasoning_tokens: to_i64(u.reasoning_tokens),
            }),
            ..Ev::new(K::Usage)
        }),
        EventKind::UserMessage { content } => one(text_ev(K::UserMessage, content)),
        EventKind::AssistantMessage { content, reasoning } => {
            one(Ev { reasoning, ..text_ev(K::AssistantMessage, content) })
        }
        EventKind::AssistantAttempt { error: Some(message), .. } => one(text_ev(K::Error, message)),
        EventKind::ToolCall { call_id, name, arguments } => one(Ev {
            call_id: Some(call_id),
            name: Some(name),
            arguments_json: Some(arguments.to_string()),
            ..Ev::new(K::ToolCall)
        }),
        EventKind::ToolResult { call_id, content, is_error } => {
            one(Ev { call_id: Some(call_id), is_error, ..text_ev(K::ToolResult, content) })
        }
        EventKind::ApprovalRequest { approval_id, call_id, tool, detail } => one(Ev {
            approval_id: Some(approval_id),
            call_id: Some(call_id),
            tool: Some(tool),
            detail,
            ..Ev::new(K::ApprovalRequest)
        }),
        EventKind::ApprovalResponse { approval_id, approved, reason } => one(Ev {
            approval_id: Some(approval_id),
            approved,
            reason,
            ..Ev::new(K::ApprovalResolved)
        }),
        EventKind::SessionTitle { title } => one(text_ev(K::TitleChanged, title)),
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
