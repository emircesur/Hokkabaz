//! ACP updates -> `hk-session` log vocabulary, so an external engine's turn replays like a
//! native one (`derive_messages` sees user/assistant/tool messages; approvals are bookkeeping).
//!
//! Stateful: assistant text streams in chunks and is logged as one `assistant/message` when a
//! tool call (or the turn end) closes it, matching how the native loop logs a step.

use std::collections::{HashMap, HashSet};

use hk_session::{EventKind, TurnEndReason};
use serde_json::{Value, json};

use crate::engine::{EngineEvent, ToolInfo};
use crate::proto::{StopReason, ToolStatus};

#[derive(Debug)]
pub struct SessionMapper {
    turn_id: String,
    step: u32,
    text: String,
    thought: String,
    /// An assistant message exists for the tool calls now being logged.
    msg_open: bool,
    /// Last event was a tool event: next text starts a new assistant message (new step).
    after_tool: bool,
    /// Calls logged and not yet resolved, in start order.
    open_calls: Vec<String>,
    resolved: HashSet<String>,
    /// Latest known content per call (updates may carry content before the final status).
    content: HashMap<String, String>,
    names: HashMap<String, String>,
}

impl SessionMapper {
    pub fn new(turn_id: impl Into<String>) -> Self {
        Self {
            turn_id: turn_id.into(),
            step: 0,
            text: String::new(),
            thought: String::new(),
            msg_open: false,
            after_tool: false,
            open_calls: Vec::new(),
            resolved: HashSet::new(),
            content: HashMap::new(),
            names: HashMap::new(),
        }
    }

    /// Events opening the turn.
    pub fn begin(&self, prompt: &str) -> Vec<EventKind> {
        vec![
            EventKind::TurnStart {
                turn_id: self.turn_id.clone(),
            },
            EventKind::UserMessage {
                content: prompt.to_owned(),
            },
            EventKind::StepStart {
                turn_id: self.turn_id.clone(),
                step: 0,
            },
        ]
    }

    pub fn push(&mut self, ev: &EngineEvent) -> Vec<EventKind> {
        let mut out = Vec::new();
        match ev {
            EngineEvent::Text(t) => {
                self.text_arrived();
                self.text.push_str(t);
            }
            EngineEvent::Thought(t) => {
                self.text_arrived();
                self.thought.push_str(t);
            }
            EngineEvent::ToolCall(info) => {
                self.flush_assistant(&mut out);
                self.after_tool = true;
                if self.names.contains_key(&info.id) {
                    // Duplicate announcement: treat as an update.
                    self.tool_update(info, &mut out);
                } else {
                    let name = info.title.clone().or_else(|| info.kind.clone()).unwrap_or_else(|| "tool".into());
                    self.names.insert(info.id.clone(), name.clone());
                    self.open_calls.push(info.id.clone());
                    out.push(EventKind::ToolCall {
                        call_id: info.id.clone(),
                        name,
                        arguments: info.raw_input.clone().unwrap_or_else(|| json!({"kind": info.kind})),
                    });
                    self.tool_update(info, &mut out);
                }
            }
            EngineEvent::ToolUpdate(info) => {
                self.after_tool = true;
                self.tool_update(info, &mut out);
            }
            EngineEvent::ApprovalRequested(p) => out.push(EventKind::ApprovalRequest {
                approval_id: p.approval_id.clone(),
                call_id: p.call_id.clone(),
                tool: p.title.clone(),
                detail: p.detail.clone(),
            }),
            EngineEvent::ApprovalResolved {
                approval_id,
                approved,
                reason,
            } => out.push(EventKind::ApprovalResponse {
                approval_id: approval_id.clone(),
                approved: *approved,
                reason: reason.clone(),
            }),
            // Plans are UI-only; the model-visible record is the text/tool trail.
            EngineEvent::Plan(_) => {}
        }
        out
    }

    /// Close the turn: flush text, fail still-open calls so the log stays well-formed.
    pub fn finish(&mut self, stop: StopReason) -> Vec<EventKind> {
        let mut out = Vec::new();
        self.flush_assistant(&mut out);
        let cancelled = stop == StopReason::Cancelled;
        for id in std::mem::take(&mut self.open_calls) {
            out.push(EventKind::ToolResult {
                call_id: id,
                content: if cancelled { "cancelled" } else { "no result" }.to_owned(),
                is_error: true,
            });
        }
        out.push(EventKind::StepEnd {
            turn_id: self.turn_id.clone(),
            step: self.step,
            finish: None,
            usage: None,
        });
        out.push(EventKind::TurnEnd {
            turn_id: self.turn_id.clone(),
            reason: end_reason(stop),
        });
        out
    }

    /// Transport/protocol failure instead of a stop reason.
    pub fn fail(&mut self) -> Vec<EventKind> {
        let mut out = self.finish(StopReason::Refusal);
        if let Some(EventKind::TurnEnd { reason, .. }) = out.last_mut() {
            *reason = TurnEndReason::Failed;
        }
        out
    }

    fn text_arrived(&mut self) {
        if self.after_tool {
            // New step after tool results: next flush logs a fresh assistant message.
            self.after_tool = false;
            self.msg_open = false;
        }
    }

    fn flush_assistant(&mut self, out: &mut Vec<EventKind>) {
        let has_text = !self.text.is_empty() || !self.thought.is_empty();
        // Tool calls must attach to an assistant message even when the agent sent no text.
        if has_text || !self.msg_open {
            let reasoning = (!self.thought.is_empty()).then(|| std::mem::take(&mut self.thought));
            out.push(EventKind::AssistantMessage {
                content: std::mem::take(&mut self.text),
                reasoning,
            });
            self.msg_open = true;
        }
    }

    fn tool_update(&mut self, info: &ToolInfo, out: &mut Vec<EventKind>) {
        if let Some(c) = &info.content {
            self.content.insert(info.id.clone(), c.clone());
        }
        let Some(status) = info.status else { return };
        if !status.is_terminal() || !self.resolved.insert(info.id.clone()) {
            return;
        }
        self.open_calls.retain(|c| c != &info.id);
        let content = self
            .content
            .remove(&info.id)
            .filter(|c| !c.is_empty())
            .or_else(|| info.raw_output.as_ref().map(raw_text))
            .unwrap_or_default();
        out.push(EventKind::ToolResult {
            call_id: info.id.clone(),
            content,
            is_error: status == ToolStatus::Failed,
        });
    }
}

fn raw_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn end_reason(s: StopReason) -> TurnEndReason {
    match s {
        StopReason::EndTurn => TurnEndReason::Completed,
        StopReason::Cancelled => TurnEndReason::Cancelled,
        StopReason::MaxTokens | StopReason::MaxTurnRequests => TurnEndReason::MaxSteps,
        StopReason::Refusal => TurnEndReason::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &str, status: Option<ToolStatus>, content: Option<&str>) -> ToolInfo {
        ToolInfo {
            id: id.into(),
            title: Some("Read".into()),
            kind: Some("read".into()),
            status,
            content: content.map(Into::into),
            raw_input: Some(json!({"path": "/a"})),
            raw_output: None,
        }
    }

    #[test]
    fn text_tool_text_sequence_is_well_formed() {
        let mut m = SessionMapper::new("t1");
        let mut log = m.begin("hi");
        for ev in [
            EngineEvent::Text("let me ".into()),
            EngineEvent::Text("look".into()),
            EngineEvent::ToolCall(tool("c1", Some(ToolStatus::Pending), None)),
            EngineEvent::ToolUpdate(tool("c1", Some(ToolStatus::Completed), Some("file body"))),
            EngineEvent::Text("done".into()),
        ] {
            log.extend(m.push(&ev));
        }
        log.extend(m.finish(StopReason::EndTurn));
        let kinds: Vec<&str> = log
            .iter()
            .map(|k| match k {
                EventKind::TurnStart { .. } => "turn",
                EventKind::UserMessage { .. } => "user",
                EventKind::StepStart { .. } => "step",
                EventKind::AssistantMessage { .. } => "asst",
                EventKind::ToolCall { .. } => "call",
                EventKind::ToolResult { .. } => "result",
                EventKind::StepEnd { .. } => "stepend",
                EventKind::TurnEnd { .. } => "turnend",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            ["turn", "user", "step", "asst", "call", "result", "asst", "stepend", "turnend"]
        );
        assert!(matches!(&log[3], EventKind::AssistantMessage { content, .. } if content == "let me look"));
        assert!(matches!(&log[5], EventKind::ToolResult { content, is_error: false, .. } if content == "file body"));
    }

    #[test]
    fn toolcall_without_text_gets_empty_assistant_and_open_calls_fail_on_cancel() {
        let mut m = SessionMapper::new("t");
        let mut log = m.push(&EngineEvent::ToolCall(tool("c1", None, None)));
        assert!(matches!(&log[0], EventKind::AssistantMessage { content, .. } if content.is_empty()));
        log.extend(m.finish(StopReason::Cancelled));
        assert!(log.iter().any(|k| matches!(k, EventKind::ToolResult { content, is_error: true, .. } if content == "cancelled")));
        assert!(matches!(log.last(), Some(EventKind::TurnEnd { reason: TurnEndReason::Cancelled, .. })));
    }

    #[test]
    fn failed_status_and_no_duplicate_results() {
        let mut m = SessionMapper::new("t");
        m.push(&EngineEvent::ToolCall(tool("c1", None, None)));
        let a = m.push(&EngineEvent::ToolUpdate(tool("c1", Some(ToolStatus::Failed), Some("boom"))));
        let b = m.push(&EngineEvent::ToolUpdate(tool("c1", Some(ToolStatus::Failed), Some("boom"))));
        assert!(matches!(&a[0], EventKind::ToolResult { is_error: true, .. }));
        assert!(b.is_empty());
        assert!(m.finish(StopReason::EndTurn).iter().all(|k| !matches!(k, EventKind::ToolResult { .. })));
    }
}
