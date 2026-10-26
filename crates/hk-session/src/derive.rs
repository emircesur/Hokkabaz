//! `derive_messages`: model history as a pure projection of the log.

use hk_llm::{Message, Role, ToolCall};

use crate::event::{EventKind, SessionEvent};

/// Build the prompt history from log events.
///
/// - `tool/call` attaches to the most recent assistant message (a lone call
///   with no assistant message gets an empty one).
/// - `compaction/summary` drops earlier non-system messages up to `through_seq`
///   and puts the summary (as a user message) right after the kept system ones.
/// - Bookkeeping events (turns, steps, attempts, headers, approvals) are not
///   model-visible.
pub fn derive_messages(events: &[SessionEvent]) -> Vec<Message> {
    // seq per message so compaction can cut by log position.
    let mut out: Vec<(u64, Message)> = Vec::new();
    let mut last_assistant: Option<usize> = None;

    for ev in events {
        match &ev.kind {
            EventKind::SystemMessage { content } => {
                out.push((ev.seq, msg(Role::System, content)));
            }
            EventKind::UserMessage { content, images } => {
                let mut m = msg(Role::User, content);
                m.images.clone_from(images);
                out.push((ev.seq, m));
            }
            EventKind::AssistantMessage { content, reasoning } => {
                let mut m = msg(Role::Assistant, content);
                m.reasoning = reasoning.clone();
                out.push((ev.seq, m));
                last_assistant = Some(out.len() - 1);
            }
            EventKind::ToolCall {
                call_id,
                name,
                arguments,
            } => {
                let idx = match last_assistant {
                    Some(i) => i,
                    None => {
                        out.push((ev.seq, msg(Role::Assistant, "")));
                        last_assistant = Some(out.len() - 1);
                        out.len() - 1
                    }
                };
                out[idx].1.tool_calls.push(ToolCall {
                    id: call_id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
            }
            EventKind::ToolResult {
                call_id, content, ..
            } => {
                let mut m = msg(Role::Tool, content);
                m.tool_call_id = Some(call_id.clone());
                out.push((ev.seq, m));
            }
            EventKind::CompactionSummary {
                summary,
                through_seq,
            } => {
                let mut kept: Vec<(u64, Message)> = Vec::with_capacity(out.len() + 1);
                let mut rest: Vec<(u64, Message)> = Vec::new();
                for (seq, m) in out.drain(..) {
                    if seq <= *through_seq {
                        if m.role == Role::System {
                            kept.push((seq, m));
                        }
                    } else {
                        rest.push((seq, m));
                    }
                }
                kept.push((ev.seq, msg(Role::User, summary)));
                kept.extend(rest);
                out = kept;
                // Dropped indices invalidate the pointer; later calls re-anchor.
                last_assistant = out.iter().rposition(|(_, m)| m.role == Role::Assistant);
            }
            EventKind::SessionStart { .. }
            | EventKind::SessionTitle { .. }
            | EventKind::TurnStart { .. }
            | EventKind::TurnEnd { .. }
            | EventKind::StepStart { .. }
            | EventKind::StepEnd { .. }
            | EventKind::AssistantAttempt { .. }
            | EventKind::RequestHeader(_)
            | EventKind::ApprovalRequest { .. }
            | EventKind::ApprovalResponse { .. } => {}
        }
    }
    out.into_iter().map(|(_, m)| m).collect()
}

fn msg(role: Role, content: &str) -> Message {
    Message { images: Vec::new(),
        role,
        content: content.to_owned(),
        reasoning: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}
