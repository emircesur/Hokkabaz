//! Connect-phase retry policy (port: dsh packages/llm `llm-retry` semantics).
//!
//! Only the request/response-headers phase is retried here. Once SSE bytes
//! flow, a failure surfaces to the caller: replaying a half-delivered answer
//! is the router's/agent's decision (it is logged as `assistant/attempt`).

use std::time::Duration;

/// Exponential backoff with a cap; honours server `Retry-After`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after the first attempt (0 = never retry).
    pub max_retries: u32,
    /// Delay before the first retry.
    pub base_delay: Duration,
    /// Upper bound for any single wait, including `Retry-After`.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(20),
        }
    }
}

impl RetryPolicy {
    /// Wait before retry number `retry` (0-based).
    pub fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        let backoff = self
            .base_delay
            .saturating_mul(2u32.saturating_pow(retry.min(16)));
        retry_after.unwrap_or(backoff).min(self.max_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let p = RetryPolicy::default();
        assert_eq!(p.delay(0, None), Duration::from_millis(500));
        assert_eq!(p.delay(2, None), Duration::from_secs(2));
        assert_eq!(p.delay(10, None), p.max_delay);
    }

    #[test]
    fn retry_after_wins_but_is_capped() {
        let p = RetryPolicy::default();
        assert_eq!(
            p.delay(0, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        assert_eq!(p.delay(0, Some(Duration::from_secs(600))), p.max_delay);
    }
}
