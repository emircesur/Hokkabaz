//! Per-model runtime tuning (P9.25): context length, GPU layers, threads, batch, flash
//! attention, KV-cache quantization, mmap/mlock. Process-wide like [`crate::gpu`]; the UI saves
//! profiles keyed by model id, `*` is the default for every model.
//!
//! Load-time fields (GPU layers, mmap, mlock) reload the model when they change; the rest apply
//! to the next request.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{PoisonError, RwLock};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeProfile {
    /// Context tokens (prompt + output); clamped to the model's training context.
    pub ctx: Option<u32>,
    /// Layers on the GPU; `None` = global GPU setting, negative = all, 0 = CPU.
    pub gpu_layers: Option<i32>,
    pub threads: Option<i32>,
    /// Prompt-processing batch size.
    pub batch: Option<u32>,
    /// `auto` (default), `on`, `off`.
    pub flash_attn: Option<String>,
    /// KV-cache element type: `f16` (default), `q8_0`, `q4_0`.
    pub kv_type: Option<String>,
    pub mmap: Option<bool>,
    pub mlock: Option<bool>,
}

impl RuntimeProfile {
    /// `self` with every field set in `over` replaced.
    pub fn overlay(&self, over: &RuntimeProfile) -> RuntimeProfile {
        RuntimeProfile {
            ctx: over.ctx.or(self.ctx),
            gpu_layers: over.gpu_layers.or(self.gpu_layers),
            threads: over.threads.or(self.threads),
            batch: over.batch.or(self.batch),
            flash_attn: over.flash_attn.clone().or_else(|| self.flash_attn.clone()),
            kv_type: over.kv_type.clone().or_else(|| self.kv_type.clone()),
            mmap: over.mmap.or(self.mmap),
            mlock: over.mlock.or(self.mlock),
        }
    }

    /// Fields that need a model reload when they change.
    #[cfg_attr(not(feature = "llama"), allow(dead_code))]
    pub(crate) fn load_key(&self) -> (Option<i32>, Option<bool>, Option<bool>) {
        (self.gpu_layers, self.mmap, self.mlock)
    }
}

static PROFILES: RwLock<BTreeMap<String, RuntimeProfile>> = RwLock::new(BTreeMap::new());
static GEN: AtomicU64 = AtomicU64::new(0);

/// Replace all profiles (from the `local_runtime_profiles` settings document).
pub fn set_all(p: BTreeMap<String, RuntimeProfile>) {
    *PROFILES.write().unwrap_or_else(PoisonError::into_inner) = p;
    GEN.fetch_add(1, Ordering::Relaxed);
}

/// Effective profile for `model`: `*` overlaid with the model's own entry.
pub fn for_model(model: &str) -> RuntimeProfile {
    let all = PROFILES.read().unwrap_or_else(PoisonError::into_inner);
    let base = all.get("*").cloned().unwrap_or_default();
    match all.get(model) {
        Some(m) => base.overlay(m),
        None => base,
    }
}

/// Bumped on every [`set_all`].
pub fn generation() -> u64 {
    GEN.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_entry_overrides_default() {
        let mut m = BTreeMap::new();
        m.insert(
            "*".into(),
            RuntimeProfile {
                ctx: Some(4096),
                threads: Some(6),
                ..Default::default()
            },
        );
        m.insert(
            "qwen".into(),
            RuntimeProfile {
                ctx: Some(16384),
                ..Default::default()
            },
        );
        set_all(m);
        let p = for_model("qwen");
        assert_eq!(p.ctx, Some(16384));
        assert_eq!(p.threads, Some(6));
        assert_eq!(for_model("other").ctx, Some(4096));
    }
}
