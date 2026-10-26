//! `hokkabaz.agent` backend: runs a full agent session (tools, modes, approvals) through the
//! instance's [`AgentRuntime`], so a flow's agent node is a normal session in the session
//! store, visible and cancellable like any other. Pending approvals surface on the session's
//! event stream; an unanswered one ends the run at the timeout.
// port: none (Hokkabaz-native; see DESIGN §3.9)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hk_flow::FlowError;
use hk_flow::nodes::ai::{AgentRunRequest, AgentRunResult, AgentRunner};
use hk_server::{AgentRuntime, CreateSession, TurnMode, TurnRequest};
use serde_json::{Map, Value, json};
use tokio::sync::broadcast::error::RecvError;

const POLL: Duration = Duration::from_secs(2);
const STEP_TEXT_MAX: usize = 800;

pub struct RuntimeAgentRunner {
    runtime: Arc<dyn AgentRuntime>,
    /// Longest a single node run may take, approvals included.
    timeout: Duration,
}

impl std::fmt::Debug for RuntimeAgentRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeAgentRunner")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl RuntimeAgentRunner {
    pub fn new(runtime: Arc<dyn AgentRuntime>) -> Self {
        Self {
            runtime,
            timeout: Duration::from_secs(15 * 60),
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
}

fn node_err(msg: impl std::fmt::Display) -> FlowError {
    FlowError::Node {
        node: "Agent".into(),
        message: msg.to_string(),
    }
}

fn clip(s: &str) -> String {
    match s.char_indices().nth(STEP_TEXT_MAX) {
        Some((i, _)) => format!("{}...", &s[..i]),
        None => s.to_owned(),
    }
}

/// Events (as JSON) logged after `after`.
async fn events_after(
    rt: &dyn AgentRuntime,
    id: &str,
    after: u64,
) -> Result<Vec<Value>, FlowError> {
    let d = rt.session(id, Some(after)).await.map_err(node_err)?;
    match serde_json::to_value(&d.events) {
        Ok(Value::Array(v)) => Ok(v),
        _ => Ok(Vec::new()),
    }
}

fn turn_end(events: &[Value]) -> Option<&str> {
    events
        .iter()
        .find(|e| e.get("type").and_then(Value::as_str) == Some("turn/end"))
        .and_then(|e| e.get("reason").and_then(Value::as_str))
}

#[async_trait]
impl AgentRunner for RuntimeAgentRunner {
    async fn run(&self, req: AgentRunRequest) -> Result<AgentRunResult, FlowError> {
        let rt = self.runtime.as_ref();
        let mode = match req.mode.as_deref() {
            None | Some("" | "build") => TurnMode::Build,
            Some("plan") => TurnMode::Plan,
            Some(other) => {
                return Err(node_err(format!(
                    "unknown agent mode `{other}`; use build or plan"
                )));
            }
        };
        let id = match req.session_id {
            Some(id) => id,
            None => {
                let title = match (&req.workflow_id, &req.execution_id) {
                    (Some(w), Some(e)) => format!("flow {w} / execution {e}"),
                    (Some(w), None) => format!("flow {w}"),
                    _ => "flow agent".to_owned(),
                };
                rt.create_session(CreateSession {
                    cwd: None,
                    model: req.model.clone(),
                    title: Some(title),
                })
                .await
                .map_err(node_err)?
                .id
            }
        };
        // Everything logged before this turn is not ours.
        let baseline = rt
            .session(&id, None)
            .await
            .map_err(node_err)?
            .events
            .iter()
            .map(|e| e.seq)
            .max()
            .unwrap_or(0);
        let mut feed = rt.subscribe();

        // The runtime has no per-turn system slot; the section rides at the top of the prompt,
        // which keeps it in the session log like everything the model sees.
        let prompt = match req.system.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(sys) => format!("{sys}\n\n{}", req.prompt),
            None => req.prompt,
        };
        rt.submit(
            &id,
            TurnRequest {
                images: Vec::new(),
                prompt,
                mode: Some(mode),
                model: req.model,
            },
        )
        .await
        .map_err(node_err)?;

        let wait = async {
            loop {
                match tokio::time::timeout(POLL, feed.recv()).await {
                    Ok(Ok(ev)) if ev.session_id == id && ev.name == "turn/end" => break,
                    Ok(Ok(_) | Err(RecvError::Lagged(_))) => continue,
                    Ok(Err(RecvError::Closed)) => return Err(node_err("agent runtime shut down")),
                    // Quiet period: the live feed can lag or drop, the log is the truth.
                    Err(_) => {
                        if turn_end(&events_after(rt, &id, baseline).await?).is_some() {
                            break;
                        }
                    }
                }
            }
            Ok(())
        };
        match tokio::time::timeout(self.timeout, wait).await {
            Ok(res) => res?,
            Err(_) => {
                let _ = rt.cancel(&id).await;
                return Err(node_err(format!(
                    "agent run exceeded {}s (waiting for an approval?)",
                    self.timeout.as_secs()
                )));
            }
        }

        let events = events_after(rt, &id, baseline).await?;
        match turn_end(&events) {
            Some("failed") => return Err(node_err("agent turn failed; see the session log")),
            Some("cancelled") => return Err(node_err("agent turn was cancelled")),
            _ => {}
        }
        let kind = |e: &Value| {
            e.get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let output = events
            .iter()
            .rev()
            .find(|e| kind(e) == "assistant/message")
            .and_then(|e| e.get("content").and_then(Value::as_str))
            .unwrap_or_default()
            .to_owned();
        let mut steps = Vec::new();
        for e in &events {
            match kind(e).as_str() {
                "tool/call" => steps.push(json!({
                    "tool": e.get("name"), "call_id": e.get("call_id"), "arguments": e.get("arguments"),
                })),
                "tool/result" => {
                    let cid = e.get("call_id");
                    let text = clip(e.get("content").and_then(Value::as_str).unwrap_or(""));
                    if let Some(Value::Object(s)) = steps.iter_mut().rev().find(|s| s.get("call_id") == cid) {
                        s.insert("result".into(), json!(text));
                        s.insert("is_error".into(), e.get("is_error").cloned().unwrap_or(json!(false)));
                    }
                }
                _ => {}
            }
        }
        let mut extra = Map::new();
        if let Some(r) = turn_end(&events) {
            extra.insert("endReason".into(), json!(r));
        }
        Ok(AgentRunResult {
            output,
            session_id: Some(id),
            steps,
            extra,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_is_char_safe() {
        assert_eq!(clip("abc"), "abc");
        assert!(clip(&"é".repeat(STEP_TEXT_MAX + 5)).ends_with("..."));
    }

    #[test]
    fn turn_end_reads_reason() {
        let ev = vec![
            json!({"type":"user/message","seq":1}),
            json!({"type":"turn/end","seq":2,"reason":"max_steps"}),
        ];
        assert_eq!(turn_end(&ev), Some("max_steps"));
        assert_eq!(turn_end(&ev[..1]), None);
    }
}
