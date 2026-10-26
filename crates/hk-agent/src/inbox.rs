use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use hk_core::{JobId, JobRegistry};
use hk_llm::CancellationToken;

use crate::approval::Gate;

#[derive(Debug, Default)]
struct Queues {
    /// New work: becomes the user messages of the next turn.
    queued: VecDeque<String>,
    /// Steering for a running turn: claimed at the next step boundary.
    injected: VecDeque<String>,
}

/// Cancel target of the running turn: its token, plus its job when jobs are wired, so a
/// cancel also goes through the job tree (events, grace) instead of around it.
#[derive(Debug, Default)]
struct TurnCancel {
    token: CancellationToken,
    job: Option<(JobRegistry, JobId)>,
}

#[derive(Debug)]
struct Shared {
    queues: Mutex<Queues>,
    cancel: Mutex<TurnCancel>,
    running: AtomicBool,
    pause: AtomicBool,
    gate: Arc<Gate>,
}

/// Cheap clonable control surface for an [`crate::Agent`], usable while it runs.
#[derive(Debug, Clone)]
pub struct AgentHandle(Arc<Shared>);

impl AgentHandle {
    pub(crate) fn new(gate: Arc<Gate>) -> Self {
        Self(Arc::new(Shared {
            queues: Mutex::default(),
            cancel: Mutex::default(),
            running: AtomicBool::new(false),
            pause: AtomicBool::new(false),
            gate,
        }))
    }

    /// Queue a user message. Starts a turn on the next `run_turn`; if a turn is already
    /// running it waits for the next one (`run_until_idle` picks it up).
    pub fn submit(&self, text: impl Into<String>) {
        self.queues().queued.push_back(text.into());
    }

    /// Steer the running turn: logged as a user message before its next step, and keeps the
    /// turn going even if the model was about to finish. Idle agents treat it like `submit`.
    pub fn inject(&self, text: impl Into<String>) {
        self.queues().injected.push_back(text.into());
    }

    /// Cancel the running turn (no-op when idle). In-flight request and tool calls are
    /// aborted; calls without a result get an error result so the log stays replayable.
    pub fn cancel(&self) {
        // Under the lock so a concurrent `adopt_job` sees it. The job goes first so
        // `job/cancel-requested` precedes the turn noticing the token.
        let c = self.turn_cancel();
        if let Some((reg, id)) = &c.job {
            reg.cancel(*id);
        }
        c.token.cancel();
    }

    fn turn_cancel(&self) -> std::sync::MutexGuard<'_, TurnCancel> {
        self.0.cancel.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// End the running turn cleanly after its current step (tool results included); when
    /// idle, the next turn pauses after its first step. A later `submit` resumes.
    pub fn pause_after_step(&self) {
        self.0.pause.store(true, Ordering::SeqCst);
    }

    pub(crate) fn take_pause(&self) -> bool {
        self.0.pause.swap(false, Ordering::SeqCst)
    }

    /// Answer a pending approval (`ApprovalRequest::approval_id`). Returns `false` when the id
    /// is unknown or was already settled, e.g. the turn was cancelled meanwhile.
    pub fn respond(&self, approval_id: &str, approved: bool, reason: Option<String>) -> bool {
        self.0.gate.respond(approval_id, approved, reason)
    }

    /// Approval ids currently waiting for an answer (for UIs attaching late).
    pub fn pending_approvals(&self) -> Vec<String> {
        self.0.gate.pending_ids()
    }

    /// A turn is in progress.
    pub fn is_running(&self) -> bool {
        self.0.running.load(Ordering::SeqCst)
    }

    fn queues(&self) -> std::sync::MutexGuard<'_, Queues> {
        self.0.queues.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Fresh token for a new turn; `cancel()` targets it from now on.
    pub(crate) fn begin_turn(&self) -> CancellationToken {
        let tok = CancellationToken::new();
        *self.turn_cancel() = TurnCancel {
            token: tok.clone(),
            job: None,
        };
        self.0.running.store(true, Ordering::SeqCst);
        tok
    }

    /// Make the turn job's token the turn token. A cancel that already hit the
    /// `begin_turn` token is carried over (under the lock, so none slips between).
    pub(crate) fn adopt_job(
        &self,
        token: CancellationToken,
        reg: JobRegistry,
        id: JobId,
    ) -> CancellationToken {
        let mut c = self.turn_cancel();
        if c.token.is_cancelled() {
            reg.cancel(id);
        }
        *c = TurnCancel {
            token: token.clone(),
            job: Some((reg, id)),
        };
        token
    }

    pub(crate) fn end_turn(&self) {
        self.turn_cancel().job = None;
        self.0.pause.store(false, Ordering::SeqCst);
        self.0.running.store(false, Ordering::SeqCst);
    }

    /// Turn start: everything pending, queued first.
    pub(crate) fn claim_all(&self) -> Vec<String> {
        let mut q = self.queues();
        let mut out: Vec<String> = q.queued.drain(..).collect();
        out.extend(q.injected.drain(..));
        out
    }

    /// Step boundary: injected messages only.
    pub(crate) fn claim_injected(&self) -> Vec<String> {
        self.queues().injected.drain(..).collect()
    }

    pub(crate) fn has_injected(&self) -> bool {
        !self.queues().injected.is_empty()
    }
}
