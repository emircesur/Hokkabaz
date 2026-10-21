use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use hk_llm::CancellationToken;

#[derive(Debug, Default)]
struct Queues {
    /// New work: becomes the user messages of the next turn.
    queued: VecDeque<String>,
    /// Steering for a running turn: claimed at the next step boundary.
    injected: VecDeque<String>,
}

#[derive(Debug)]
struct Shared {
    queues: Mutex<Queues>,
    cancel: Mutex<CancellationToken>,
    running: AtomicBool,
}

/// Cheap clonable control surface for an [`crate::Agent`], usable while it runs.
#[derive(Debug, Clone)]
pub struct AgentHandle(Arc<Shared>);

impl AgentHandle {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Shared {
            queues: Mutex::default(),
            cancel: Mutex::new(CancellationToken::new()),
            running: AtomicBool::new(false),
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
        self.0
            .cancel
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancel();
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
        *self.0.cancel.lock().unwrap_or_else(PoisonError::into_inner) = tok.clone();
        self.0.running.store(true, Ordering::SeqCst);
        tok
    }

    pub(crate) fn end_turn(&self) {
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
