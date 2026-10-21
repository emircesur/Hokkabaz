//! Loop guards: repeat-tool detector and the deadline helpers shared by the step/tool phases.
// port: dsh packages/guard (repeat-tool reminder, step/turn timeouts, cost cap)

use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;

use crate::config::Guards;

/// What the repeat detector wants done before the next request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RepeatAction {
    None,
    /// Log this as a user message so the model sees it.
    Remind(String),
    /// End the turn as failed.
    Abort(String),
}

/// Counts consecutive identical tool calls (same name and arguments) within a turn. A
/// different call in between resets it: only genuine loops trip it, not re-reads over time.
#[derive(Debug, Default)]
pub(crate) struct RepeatGuard {
    last: Option<String>,
    count: u32,
    /// Count at which the last reminder was issued, so one loop gets one reminder per window.
    reminded_at: u32,
}

impl RepeatGuard {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn observe(&mut self, name: &str, args: &Value) {
        let sig = format!("{name}\u{0}{args}");
        if self.last.as_deref() == Some(sig.as_str()) {
            self.count += 1;
        } else {
            self.last = Some(sig);
            self.count = 1;
            self.reminded_at = 0;
        }
    }

    pub(crate) fn check(&mut self, g: &Guards) -> RepeatAction {
        let name = self
            .last
            .as_deref()
            .and_then(|s| s.split('\u{0}').next())
            .unwrap_or_default()
            .to_owned();
        if g.repeat_abort_after > 0 && self.count >= g.repeat_abort_after {
            return RepeatAction::Abort(format!(
                "repeat guard: `{name}` called {} times in a row with identical arguments",
                self.count
            ));
        }
        let n = g.repeat_remind_after;
        if n > 0 && self.count >= n && self.count - self.reminded_at >= n {
            self.reminded_at = self.count;
            return RepeatAction::Remind(format!(
                "Reminder: you called `{name}` {} times in a row with identical arguments. \
                 The result will not change. Try a different approach or answer with what you have.",
                self.count
            ));
        }
        RepeatAction::None
    }
}

/// A deadline plus the name used in the failure text.
pub(crate) type Limit = (Instant, &'static str);

/// Earliest of the turn deadline and a phase-local timeout starting now.
pub(crate) fn limit(
    turn: Option<Instant>,
    local: Option<(Duration, &'static str)>,
) -> Option<Limit> {
    let local = local.map(|(d, what)| (Instant::now() + d, what));
    match (turn.map(|t| (t, "turn timeout")), local) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// Resolves at `at`; never when there is no deadline.
pub(crate) async fn until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reminds_every_window_aborts_at_limit_and_resets_on_change() {
        let g = Guards {
            repeat_remind_after: 2,
            repeat_abort_after: 5,
            ..Guards::default()
        };
        let mut r = RepeatGuard::default();
        let a = json!({"p": 1});
        r.observe("read", &a);
        assert_eq!(r.check(&g), RepeatAction::None);
        r.observe("read", &a);
        assert!(matches!(r.check(&g), RepeatAction::Remind(m) if m.contains("`read`")));
        // same count, no new call: no second reminder
        assert_eq!(r.check(&g), RepeatAction::None);
        r.observe("read", &a);
        assert_eq!(r.check(&g), RepeatAction::None);
        r.observe("read", &a);
        assert!(matches!(r.check(&g), RepeatAction::Remind(_)));
        r.observe("read", &a);
        assert!(matches!(r.check(&g), RepeatAction::Abort(_)));

        // different args break the streak
        r.observe("read", &json!({"p": 2}));
        assert_eq!(r.check(&g), RepeatAction::None);
    }

    #[test]
    fn zero_disables() {
        let g = Guards {
            repeat_remind_after: 0,
            repeat_abort_after: 0,
            ..Guards::default()
        };
        let mut r = RepeatGuard::default();
        for _ in 0..20 {
            r.observe("x", &json!({}));
        }
        assert_eq!(r.check(&g), RepeatAction::None);
    }
}
