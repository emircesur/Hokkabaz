//! Per-token token bucket. Hand-rolled (no extra dependency): one bucket per authenticated
//! token, charged once per request after auth, so unauthenticated probes can't evict real
//! buckets. State is in-process and resets on restart.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;


/// Bucket shape: `burst` requests at once, refilled at `per_sec`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimit {
    pub burst: u32,
    pub per_sec: f64,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self { burst: 120, per_sec: 30.0 }
    }
}

/// Bucket empty; retry after this many seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Throttled(pub u64);

#[derive(Debug)]
pub struct RateLimiter {
    cfg: Option<RateLimit>,
    buckets: Mutex<HashMap<usize, (f64, Instant)>>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(Some(RateLimit::default()))
    }
}

impl RateLimiter {
    /// `None` disables limiting.
    pub fn new(cfg: Option<RateLimit>) -> Self {
        Self { cfg, buckets: Mutex::default() }
    }

    pub fn check(&self, key: usize) -> Result<(), Throttled> {
        self.check_at(key, Instant::now())
    }

    pub(crate) fn check_at(&self, key: usize, now: Instant) -> Result<(), Throttled> {
        let Some(cfg) = self.cfg else { return Ok(()) };
        let cap = f64::from(cfg.burst.max(1));
        let rate = cfg.per_sec.max(f64::MIN_POSITIVE);
        let mut map = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        let (tokens, last) = map.entry(key).or_insert((cap, now));
        let dt = now.saturating_duration_since(*last).as_secs_f64();
        *tokens = (*tokens + dt * rate).min(cap);
        *last = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            let wait = ((1.0 - *tokens) / rate).ceil().max(1.0);
            Err(Throttled(wait as u64))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn burst_then_429_then_refill() {
        let l = RateLimiter::new(Some(RateLimit { burst: 2, per_sec: 1.0 }));
        let t0 = Instant::now();
        assert!(l.check_at(1, t0).is_ok());
        assert!(l.check_at(1, t0).is_ok());
        match l.check_at(1, t0) {
            Err(Throttled(s)) => assert_eq!(s, 1),
            other => panic!("expected 429, got {other:?}"),
        }
        // another token has its own bucket
        assert!(l.check_at(2, t0).is_ok());
        assert!(l.check_at(1, t0 + Duration::from_millis(1100)).is_ok());
        assert!(l.check_at(1, t0 + Duration::from_millis(1100)).is_err());
    }

    #[test]
    fn disabled_never_limits() {
        let l = RateLimiter::new(None);
        for _ in 0..1000 {
            assert!(l.check(0).is_ok());
        }
    }
}
