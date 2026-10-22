//! Provider settings, secrets wiring and the hot-reloadable router. One implementation behind
//! both the in-process app core (`hk-ffi`) and `hokkabazd`, so what the app configures locally is
//! configurable on an instance too.

mod error;
mod hub;
pub mod settings;

pub use error::ProvidersError;
pub use hub::{
    HubOptions, ModelsView, Offline, Preferences, ProviderHub, ProviderView, SettingsView,
};
pub use settings::{PRESETS, Preset, ProviderInput, Stored, StoredProvider};
