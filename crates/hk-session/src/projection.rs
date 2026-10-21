//! Incremental folds over the event stream (UI state such as the cost meter).

use hk_llm::Usage;

use crate::event::{EventKind, SessionEvent};

/// State derived by folding events one at a time. Apply is incremental, so UIs
/// keep one instance and feed it live events instead of re-reading the log.
pub trait Projection: Default {
    /// Fold one event into the state.
    fn apply(&mut self, event: &SessionEvent);
}

/// Fold a whole log into a projection.
pub fn fold<P: Projection>(events: &[SessionEvent]) -> P {
    let mut p = P::default();
    for e in events {
        p.apply(e);
    }
    p
}

/// Token meter: sum of per-step usage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageMeter {
    /// Accumulated usage.
    pub usage: Usage,
    /// Steps that reported usage.
    pub steps: u64,
}

impl Projection for UsageMeter {
    fn apply(&mut self, event: &SessionEvent) {
        if let EventKind::StepEnd { usage: Some(u), .. } = &event.kind {
            self.usage.accumulate(u);
            self.steps += 1;
        }
    }
}
