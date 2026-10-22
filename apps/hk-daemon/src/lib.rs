//! `hokkabazd` as a library: assembly of the headless instance (store, auth, agent runtime,
//! flow runtime) so integration tests drive the same wiring as the binary.

pub mod agent_runner;
pub mod daemon;
pub mod flow_runtime;
#[cfg(feature = "plugins")]
pub mod plugins;
pub mod scrub;
pub mod secrets;
pub mod wait_store;

pub use daemon::{Assembled, Opened, Unconfigured, assemble, open, open_with};
pub use flow_runtime::{FlowConfig, FlowRuntime};
