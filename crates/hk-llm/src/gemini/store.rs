//! Replay store for `thoughtSignature`s.
//!
//! Gemini 2.5/3 thinking models attach an opaque `thoughtSignature` to the
//! `functionCall` part of a tool turn (on parallel calls: the first one). Gemini
//! 3 rejects the follow-up request (400) when the signature is missing from the
//! replayed call; 2.5 only degrades reasoning quality. [`crate::StreamEvent`]
//! and [`crate::Message`] have no slot for it, so, like the Anthropic adapter,
//! the signature is kept in memory keyed by the (synthesised) tool-call id and
//! re-attached when the same `Gemini` instance serves the follow-up. On a miss
//! (restart, resumed session) the request builder falls back to Google's
//! documented bypass value, see `request::SKIP_SIGNATURE`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

const CAPACITY: usize = 512;

#[derive(Debug, Default)]
struct Inner {
    map: HashMap<String, Arc<str>>,
    order: VecDeque<String>,
}

/// Bounded, shared map `tool-call id -> thoughtSignature`.
#[derive(Debug, Clone, Default)]
pub(crate) struct SignatureStore(Arc<Mutex<Inner>>);

impl SignatureStore {
    pub(crate) fn put(&self, tool_id: &str, signature: &str) {
        if signature.is_empty() {
            return;
        }
        let Ok(mut g) = self.0.lock() else { return };
        if g.map.insert(tool_id.to_owned(), signature.into()).is_none() {
            g.order.push_back(tool_id.to_owned());
        }
        while g.order.len() > CAPACITY {
            if let Some(old) = g.order.pop_front() {
                g.map.remove(&old);
            }
        }
    }

    pub(crate) fn get(&self, tool_id: &str) -> Option<Arc<str>> {
        self.0.lock().ok()?.map.get(tool_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_store_evicts_oldest() {
        let s = SignatureStore::default();
        for i in 0..(CAPACITY + 5) {
            s.put(&format!("t{i}"), "sig");
        }
        assert!(s.get("t0").is_none());
        assert!(s.get(&format!("t{}", CAPACITY + 4)).is_some());
        s.put("empty", "");
        assert!(s.get("empty").is_none());
    }
}
