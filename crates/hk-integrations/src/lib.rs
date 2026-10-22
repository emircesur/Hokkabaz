//! hk-integrations: framework for built-in integration nodes, plus the integrations
//! themselves, each behind an `int-<name>` cargo feature.
//!
//! - [`credentials`]: n8n credential types (fields, authenticate, test), the registry the
//!   REST schema route reads, and the generic types (`httpHeaderAuth`, `httpBasicAuth`,
//!   `httpBearerAuth`, `httpQueryAuth`, `httpCustomAuth`, `oAuth2Api` shape, `apiKey`).
//! - [`describe`]: builders for n8n node descriptions and the routing-lite block.
//! - [`rest`]: [`RestNode`](rest::RestNode), a declarative-style HTTP node driven by the
//!   description's routing; a typical integration is ~50 lines (see `ntfy.rs`).
//! - [`poll`]: polling trigger base and a `TriggerHost` wrapper that arms it.
//!
//! Secrets: values come from hk-flow's `CredentialResolver` (hk-store secrets behind it),
//! are applied to one request and dropped; no type here logs or stores them.
//! Android: everything is pure Rust over reqwest/rustls. An integration needing a native
//! dependency must gate it with `cfg(not(target_os = "android"))` and stay out of the
//! default feature set.

pub mod credentials;
pub mod describe;
pub mod poll;
pub mod rest;
pub mod template;

#[cfg(feature = "int-ntfy")]
pub mod ntfy;

#[cfg(any(feature = "int-github", feature = "int-gitea", feature = "int-n8n"))]
pub mod pathparam;
#[cfg(feature = "int-git")]
pub mod git;
#[cfg(feature = "int-gitea")]
pub mod gitea;
#[cfg(feature = "int-github")]
pub mod github;
#[cfg(feature = "int-n8n")]
pub mod n8n;

#[cfg(any(
    feature = "int-telegram",
    feature = "int-matrix",
    feature = "int-gotify",
    feature = "int-email",
    feature = "int-rss"
))]
pub(crate) mod kit;
#[cfg(feature = "int-telegram")]
pub mod telegram;
#[cfg(feature = "int-matrix")]
pub mod matrix;
#[cfg(feature = "int-gotify")]
pub mod gotify;
#[cfg(feature = "int-email")]
pub mod email;
#[cfg(feature = "int-rss")]
pub mod rss;

pub use credentials::{
    AppliedAuth, Authenticate, CredentialError, CredentialType, CredentialTypes, builtin_types,
};
pub use poll::{PollRegistry, PollingHost, PollingTrigger};

use hk_flow::{FlowError, NodeRegistry};

/// Register the generic credential types and every integration enabled by cargo
/// features. Polling triggers are not included (they need a host): use
/// [`register_integrations_with_polling`].
pub fn register_integrations(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    register_integrations_with_polling(nodes, creds, &mut PollRegistry::new())
}

/// Like [`register_integrations`], also collecting polling triggers into `polls` for a
/// [`PollingHost`].
// `polls` is for polling integrations; none are enabled yet
#[allow(unused_variables)]
pub fn register_integrations_with_polling(
    nodes: &mut NodeRegistry,
    creds: &CredentialTypes,
    polls: &mut PollRegistry,
) -> Result<(), FlowError> {
    for t in builtin_types() {
        creds.register_if_absent(t);
    }
    #[cfg(feature = "int-ntfy")]
    ntfy::register(nodes, creds)?;
    #[cfg(feature = "int-github")]
    github::register(nodes, creds, polls)?;
    #[cfg(feature = "int-gitea")]
    gitea::register(nodes, creds)?;
    #[cfg(feature = "int-git")]
    git::register(nodes, creds)?;
    #[cfg(feature = "int-n8n")]
    n8n::register(nodes, creds)?;
    #[cfg(feature = "int-telegram")]
    telegram::register(nodes, creds, polls)?;
    #[cfg(feature = "int-matrix")]
    matrix::register(nodes, creds)?;
    #[cfg(feature = "int-gotify")]
    gotify::register(nodes, creds)?;
    #[cfg(feature = "int-email")]
    email::register(nodes, creds, polls)?;
    #[cfg(feature = "int-rss")]
    rss::register(nodes, creds, polls)?;
    Ok(())
}
