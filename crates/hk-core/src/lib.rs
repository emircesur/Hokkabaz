//! hk-core: Cordis-like plugin host. Plugins mount into a `Host`, receive a scoped `Ctx`,
//! and register services / event handlers whose `Effect`s unwind in reverse order on unload.
// port: design §3.2 (concept from cordis scope/effect model; no dsh/opencode source ported)

mod bus;
mod effect;
mod error;
mod host;
mod services;

pub use bus::{Event, EventBus, INTERCEPTABLE_DOMAINS, Rejected, Verdict};
pub use effect::{Effect, EffectHandle, Scope};
pub use error::CoreError;
pub use host::{BoxFuture, Ctx, Host, Plugin, PluginId};
pub use services::ServiceRegistry;
