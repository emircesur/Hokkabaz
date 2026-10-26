//! Approval gate: a `tools/pre-execute` interceptor that applies the [`Mode`] policy and, for
//! `Ask` decisions, parks the call until a UI answers through [`crate::AgentHandle::respond`].
//!
//! The interceptor can't touch the session log (the agent owns the writer), so it reports over
//! a channel the agent drains while a tool runs; request and response are therefore logged in
//! order, before the tool result.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use hk_core::Verdict;
use hk_tools::{Risk, ToolPreExecute};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::mode::{Decision, Mode};

const DETAIL_MAX: usize = 300;

/// Gate -> agent traffic.
#[derive(Debug)]
pub(crate) enum GateEvent {
    Request {
        approval_id: String,
        call_id: String,
        tool: String,
        risk: Risk,
        detail: Option<String>,
    },
    Response {
        approval_id: String,
        approved: bool,
        reason: Option<String>,
    },
}

#[derive(Debug)]
struct Answer {
    approved: bool,
    reason: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Gate {
    mode: Mutex<Mode>,
    policy: Mutex<std::collections::BTreeMap<String, String>>,
    dry_run: AtomicBool,
    pending: Mutex<HashMap<String, oneshot::Sender<Answer>>>,
    tx: mpsc::UnboundedSender<GateEvent>,
    next: AtomicU64,
}

/// Removes the pending entry if the waiting call is dropped (turn cancelled).
struct PendingGuard<'a>(&'a Gate, String);
impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.0.pending().remove(&self.1);
    }
}

impl Gate {
    pub(crate) fn new(mode: Mode) -> (Arc<Self>, mpsc::UnboundedReceiver<GateEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let gate = Arc::new(Self {
            mode: Mutex::new(mode),
            policy: Mutex::default(),
            dry_run: AtomicBool::new(false),
            pending: Mutex::default(),
            tx,
            next: AtomicU64::new(0),
        });
        (gate, rx)
    }

    pub(crate) fn mode(&self) -> Mode {
        *self.mode.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn set_policy(&self, p: std::collections::BTreeMap<String, String>) {
        *self.policy.lock().unwrap_or_else(PoisonError::into_inner) = p;
    }

    pub(crate) fn set_dry_run(&self, on: bool) {
        self.dry_run.store(on, Ordering::Relaxed);
    }

    pub(crate) fn set_mode(&self, m: Mode) {
        *self.mode.lock().unwrap_or_else(PoisonError::into_inner) = m;
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<Answer>>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Answer a pending approval. `false` when the id is unknown or already settled.
    pub(crate) fn respond(
        &self,
        approval_id: &str,
        approved: bool,
        reason: Option<String>,
    ) -> bool {
        let Some(tx) = self.pending().remove(approval_id) else {
            return false;
        };
        // Queue the log entry before waking the call so it precedes the tool result.
        let _ = self.tx.send(GateEvent::Response {
            approval_id: approval_id.to_owned(),
            approved,
            reason: reason.clone(),
        });
        tx.send(Answer { approved, reason }).is_ok()
    }

    pub(crate) fn pending_ids(&self) -> Vec<String> {
        let mut v: Vec<_> = self.pending().keys().cloned().collect();
        v.sort();
        v
    }

    /// The interceptor body.
    pub(crate) async fn check(&self, e: ToolPreExecute) -> Verdict<ToolPreExecute> {
        if self.dry_run.load(Ordering::Relaxed) && e.risk != Risk::Safe {
            return Verdict::Reject(format!(
                "dry run: `{}` was not executed. It would have run with {}",
                e.tool,
                detail(&e.args).unwrap_or_else(|| "no arguments".into())
            ));
        }
                let mode = match self.mode() {
            Mode::Plan => "plan",
            _ => "build",
        };
        let rule = self
            .policy
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&format!("{}:{mode}", e.tool))
            .cloned();
        let decision = match rule.as_deref() {
            Some("allow") => Decision::Allow,
            Some("ask") => Decision::Ask,
            Some("deny") => {
                Decision::Deny(format!("`{}` is denied by your approval policy", e.tool))
            }
            _ => self.mode().decide(&e.tool, e.risk),
        };
        match decision {
            Decision::Allow => Verdict::Continue,
            Decision::Deny(reason) => Verdict::Reject(reason),
            Decision::Ask => self.ask(&e).await,
        }
    }

    async fn ask(&self, e: &ToolPreExecute) -> Verdict<ToolPreExecute> {
        let approval_id = format!("appr_{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        self.pending().insert(approval_id.clone(), tx);
        let _guard = PendingGuard(self, approval_id.clone());
        let _ = self.tx.send(GateEvent::Request {
            approval_id,
            call_id: e.call_id.clone(),
            tool: e.tool.clone(),
            risk: e.risk,
            detail: detail(&e.args),
        });
        match rx.await {
            Ok(Answer { approved: true, .. }) => Verdict::Continue,
            Ok(Answer { reason, .. }) => Verdict::Reject(match reason {
                Some(r) if !r.is_empty() => format!("denied by user: {r}"),
                _ => "denied by user".to_owned(),
            }),
            Err(_) => Verdict::Reject("approval abandoned".to_owned()),
        }
    }
}

/// Human-readable summary of what the call would do: the command / path / url when the args
/// have one, else compact JSON. Capped so an approval card stays small.
fn detail(args: &Value) -> Option<String> {
    let s = ["command", "path", "url", "pattern"]
        .iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| args.to_string());
    if s.is_empty() || s == "{}" {
        return None;
    }
    Some(match s.char_indices().nth(DETAIL_MAX) {
        Some((i, _)) => format!("{}...", &s[..i]),
        None => s,
    })
}
