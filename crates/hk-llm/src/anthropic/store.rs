//! Replay store for thinking blocks.
//!
//! Anthropic requires the signed `thinking` / `redacted_thinking` blocks of an
//! assistant turn to be sent back verbatim when that turn made tool calls
//! (and thinking is on). [`crate::StreamEvent`] and [`crate::Message`] have no
//! slot for the signature, and extending them would break every exhaustive
//! match / struct literal in other crates. So the adapter keeps the blocks
//! itself, keyed by the turn's tool-call ids (unique, and already in the
//! session log). A miss (process restarted, other provider produced the turn)
//! makes the request builder switch thinking off for that request instead of
//! sending an invalid history.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// One signed reasoning block, in the order the model produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ThinkingBlock {
    Thinking { thinking: String, signature: String },
    Redacted { data: String },
}

const CAPACITY: usize = 512;

#[derive(Debug, Default)]
struct Inner {
    map: HashMap<String, Arc<Vec<ThinkingBlock>>>,
    order: VecDeque<String>,
}

/// Bounded, shared map `tool_use id -> thinking blocks of that turn`.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReasoningStore(Arc<Mutex<Inner>>);

impl ReasoningStore {
    pub(crate) fn put(&self, tool_ids: &[String], blocks: Vec<ThinkingBlock>) {
        if tool_ids.is_empty() || blocks.is_empty() {
            return;
        }
        let blocks = Arc::new(blocks);
        let Ok(mut g) = self.0.lock() else { return };
        for id in tool_ids {
            if g.map.insert(id.clone(), blocks.clone()).is_none() {
                g.order.push_back(id.clone());
            }
        }
        while g.order.len() > CAPACITY {
            if let Some(old) = g.order.pop_front() {
                g.map.remove(&old);
            }
        }
    }

    pub(crate) fn get(&self, tool_id: &str) -> Option<Arc<Vec<ThinkingBlock>>> {
        self.0.lock().ok()?.map.get(tool_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_store_evicts_oldest() {
        let s = ReasoningStore::default();
        let b = vec![ThinkingBlock::Redacted { data: "x".into() }];
        for i in 0..(CAPACITY + 5) {
            s.put(&[format!("t{i}")], b.clone());
        }
        assert!(s.get("t0").is_none());
        assert!(s.get(&format!("t{}", CAPACITY + 4)).is_some());
    }
}
