//! hk-local: on-device LLMs.
//!
//! * [`catalog`]: curated GGUF models with sizes and checksums, device RAM and a fit check.
//! * [`store`]: installed models on disk; resumable, checksum-verified downloads.
//! * [`toolcall`]: streaming parser for `<tool_call>` blocks (small models call tools in-text).
//! * [`whisper`]: speech models (ggml) for local transcription, same store.
//! * [`engine`] (feature `llama`): a [`hk_llm::Provider`] (`local`) on llama.cpp.
//!
//! Without the `llama` feature the manager still works (browse, download, delete); the provider
//! then reports that this build has no inference engine.

pub mod catalog;
#[cfg(feature = "llama")]
pub mod engine;
mod error;
pub mod gpu;
pub mod profile;
pub mod replace;
pub mod store;
pub mod toolcall;
pub mod whisper;

pub use catalog::{CatalogEntry, Device, Fit};
pub use error::{LocalError, Result};
pub use store::{DownloadProgress, InstalledModel, ModelStore};

/// Free the loaded on-device model(s); returns how many were unloaded.
pub fn unload_all() -> usize {
    #[cfg(feature = "llama")]
    {
        engine::unload_all()
    }
    #[cfg(not(feature = "llama"))]
    {
        0
    }
}

/// Ids of on-device models currently in memory.
pub fn loaded_models() -> Vec<String> {
    #[cfg(feature = "llama")]
    {
        engine::loaded_models()
    }
    #[cfg(not(feature = "llama"))]
    {
        Vec::new()
    }
}

/// Provider id registered with the router; model refs look like `local/<model id>`.
pub const PROVIDER_ID: &str = "local";
