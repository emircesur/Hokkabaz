//! hk-core: Cordis-like plugin host. Plugins mount into a `Host`, receive a scoped `Ctx`,
//! and register services / event handlers whose `Effect`s unwind in reverse order on unload.
// port: design §3.2 (concept from cordis scope/effect model; no dsh/opencode source ported)

pub mod ai_settings;
mod bus;
mod config;
mod effect;
mod error;
mod host;
mod jobs;
mod services;

pub use ai_settings::{
    AiSettings, ModelCapsLike, Preset, ReasoningEffort, ReasoningVisibility, Resolved,
    Scope as SettingsScope, SettingsStack, builtin_presets,
};
pub use bus::{Event, EventBus, INTERCEPTABLE_DOMAINS, Rejected, Verdict};
pub use config::{Config, ConfigError, Layer, LoadOptions, Profile};
pub use effect::{Effect, EffectHandle, Scope};
pub use error::CoreError;
pub use host::{BoxFuture, Ctx, Host, Plugin, PluginId};
pub use jobs::{
    DEFAULT_GRACE, JobEvent, JobHandle, JobId, JobInfo, JobKind, JobRegistry, JobSpec, JobState,
    JobsPlugin, Progress,
};
pub use services::ServiceRegistry;
