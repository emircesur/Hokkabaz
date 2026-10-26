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
    /// GPU when there are several: 1-based position among GPUs (see [`devices`]); 0 or below =
    /// auto (the discrete GPU with the most memory, so an Intel iGPU never wins over NVIDIA).
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

/// One GPU the inference backend can use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuDevice {
    /// 1-based position among GPUs (what [`GpuSettings::device`] takes).
    pub position: i32,
    /// Backend device index (llama.cpp / ggml).
    pub index: usize,
    pub name: String,
    pub description: String,
    pub memory_total: u64,
    /// Integrated GPU (shares system memory).
    pub integrated: bool,
}

/// GPUs visible to the llama.cpp backend of this build (empty without a GPU backend).
pub fn devices() -> Vec<GpuDevice> {
    #[cfg(feature = "llama")]
    {
        use llama_cpp_2::LlamaBackendDeviceType as T;
        let mut pos = 0;
        llama_cpp_2::list_llama_ggml_backend_devices()
            .into_iter()
            .filter(|d| matches!(d.device_type, T::Gpu | T::IntegratedGpu))
            .map(|d| {
                pos += 1;
                GpuDevice {
                    position: pos,
                    index: d.index,
                    name: d.name,
                    description: d.description,
                    memory_total: d.memory_total as u64,
                    integrated: d.device_type == T::IntegratedGpu,
                }
            })
            .collect()
    }
    #[cfg(not(feature = "llama"))]
    {
        Vec::new()
    }
}

/// The device to use: the chosen position, else the discrete GPU with the most memory, else
/// the first integrated one.
pub fn pick(devices: &[GpuDevice], device: i32) -> Option<&GpuDevice> {
    if device > 0
        && let Some(d) = devices.iter().find(|d| d.position == device)
    {
        return Some(d);
    }
    devices
        .iter()
        .filter(|d| !d.integrated)
        .max_by_key(|d| d.memory_total)
        .or_else(|| devices.first())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn d(position: i32, mem: u64, integrated: bool) -> GpuDevice {
        GpuDevice {
            position,
            index: position as usize,
            name: String::new(),
            description: String::new(),
            memory_total: mem,
            integrated,
        }
    }

    #[test]
    fn auto_prefers_discrete() {
        let all = [d(1, 16 << 30, true), d(2, 8 << 30, false)];
        assert_eq!(pick(&all, 0).unwrap().position, 2);
        assert_eq!(pick(&all, 1).unwrap().position, 1);
        assert_eq!(pick(&[d(1, 1, true)], 0).unwrap().position, 1);
        assert!(pick(&[], 0).is_none());
    }
}
