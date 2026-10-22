//! GPU offload for on-device models. Process-wide: one setting for every local model, applied on
//! the next model load.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

/// GPU backend compiled into llama.cpp in this build, if any.
pub const BACKEND: Option<&str> = if cfg!(feature = "llama-vulkan") {
    Some("vulkan")
} else if cfg!(feature = "llama-cuda") {
    Some("cuda")
} else if cfg!(all(feature = "llama", target_os = "macos")) {
    Some("metal")
} else {
    None
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuSettings {
    /// Offload to the GPU when the build has a backend. A load that fails on the GPU is retried
    /// on the CPU.
    pub enabled: bool,
    /// GPU index when there are several (0 = first).
    pub device: i32,
    /// Layers to offload; `None` = all (the model must fit in video memory).
    pub layers: Option<u32>,
}

impl Default for GpuSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            device: 0,
            layers: None,
        }
    }
}

impl GpuSettings {
    /// Layers to offload for this build (0 = CPU).
    pub fn effective_layers(&self) -> u32 {
        if self.enabled && BACKEND.is_some() {
            self.layers.unwrap_or(u32::MAX)
        } else {
            0
        }
    }
}

static CURRENT: Mutex<Option<GpuSettings>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn get() -> GpuSettings {
    CURRENT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .unwrap_or_default()
}

/// Change the setting; loaded models are reloaded with it on their next use.
pub fn set(g: GpuSettings) {
    *CURRENT.lock().unwrap_or_else(PoisonError::into_inner) = Some(g);
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Bumped by every [`set`]; a model loaded under an older generation is stale.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}
