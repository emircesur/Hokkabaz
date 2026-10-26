//! hk-plugin: WASM plugins for Hokkabaz.
//!
//! A plugin is a WebAssembly component implementing `wit/hokkabaz-plugin.wit` plus a
//! `plugin.toml` that declares what it may do. The host ([`PluginHost`], feature
//! `wasm-plugins`) runs every call in a fresh sandboxed instance with limits on time and memory;
//! host services (HTTP, key/value, secrets, events, files) are checked against the manifest's
//! [`Capabilities`] on every call.
//!
//! * [`manifest`]: `plugin.toml` and capability checks.
//! * [`store`]: installed plugins on disk (install / list / remove).
//! * [`registry`]: the plugin index (`index.json`), hash-verified downloads.
//! * [`template`]: template packages (flows, agents, prompts, ...) listed in the same index.
//! * [`host`] (feature `wasm-plugins`): the runtime; [`tool`] / [`node`]: adapters into the
//!   agent's tool registry and the flow engine's node registry.

pub mod manifest;
pub mod registry;
pub mod store;
pub mod template;

#[cfg(feature = "wasm-plugins")]
pub mod host;
#[cfg(feature = "wasm-plugins")]
pub mod node;
#[cfg(feature = "wasm-plugins")]
pub mod tool;

mod error;

pub use error::{PluginError, Result};
#[cfg(feature = "wasm-plugins")]
pub use host::{Description, HostServices, LoadedPlugin, PluginHost, SecretLookup, TriggerDecl};
pub use manifest::{Capabilities, FsGrant, Manifest};
pub use template::{
    InputKind, InstantiatedTemplate, TemplateInput, TemplateKind, TemplateManifest, TemplatePackage,
};
