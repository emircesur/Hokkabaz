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
//! - `sqlite`, `postgres`, `redis`, `s3`, `searxng`, `ollama`: data and infra nodes. `searxng`
//!   also exports [`searxng_web_search_tool`], a `web_search` agent tool (hk-tools `Tool`).
//!
//! Secrets: values come from hk-flow's `CredentialResolver` (hk-store secrets behind it),
//! are applied to one request and dropped; no type here logs or stores them.
//! Android: everything is pure Rust over reqwest/rustls. An integration needing a native
//! dependency must gate it with `cfg(not(target_os = "android"))` and stay out of the
//! default feature set.

pub mod credentials;
pub mod describe;
pub(crate) mod oauth;
#[cfg(feature = "int-saas")]
pub mod oauth_flow;
pub mod poll;
pub mod rest;
pub mod template;

#[cfg(feature = "int-ntfy")]
pub mod ntfy;

#[cfg(feature = "int-saas")]
pub mod declarative;
#[cfg(feature = "int-saas")]
mod specs;

#[cfg(feature = "int-ftp")]
pub mod ftp;
#[cfg(feature = "int-git")]
pub mod git;
#[cfg(feature = "int-gitea")]
pub mod gitea;
#[cfg(feature = "int-github")]
pub mod github;
#[cfg(feature = "int-kafka")]
pub mod kafka;
#[cfg(feature = "int-n8n")]
pub mod n8n;
#[cfg(feature = "int-n8n")]
pub mod n8n_remote;
#[cfg(any(
    feature = "int-github",
    feature = "int-gitea",
    feature = "int-n8n",
    feature = "int-saas"
))]
pub mod pathparam;
#[cfg(feature = "int-slack")]
pub mod slack;

#[cfg(feature = "int-email")]
pub mod email;
#[cfg(feature = "int-gotify")]
pub mod gotify;
#[cfg(any(
    feature = "int-telegram",
    feature = "int-matrix",
    feature = "int-gotify",
    feature = "int-email",
    feature = "int-rss",
    feature = "int-sqlite",
    feature = "int-postgres",
    feature = "int-redis",
    feature = "int-s3",
    feature = "int-searxng",
    feature = "int-ollama",
    feature = "int-mysql",
    feature = "int-mqtt",
    feature = "int-rabbitmq",
    feature = "int-ssh",
    feature = "int-kafka",
    feature = "int-mongodb",
    feature = "int-ftp"
))]
pub(crate) mod kit;
#[cfg(feature = "int-matrix")]
pub mod matrix;
#[cfg(all(feature = "int-mongodb", not(target_os = "android")))]
pub mod mongodb;
#[cfg(feature = "int-mqtt")]
pub mod mqtt;
#[cfg(feature = "int-mysql")]
pub mod mysql;
#[cfg(feature = "int-ollama")]
pub mod ollama;
#[cfg(feature = "int-postgres")]
pub mod postgres;
#[cfg(feature = "int-rabbitmq")]
pub mod rabbitmq;
#[cfg(feature = "int-redis")]
pub mod redis;
#[cfg(feature = "int-rss")]
pub mod rss;
#[cfg(feature = "int-s3")]
pub mod s3;
#[cfg(feature = "int-searxng")]
pub mod searxng;
#[cfg(any(feature = "int-s3", feature = "int-saas"))]
pub(crate) mod sigv4;
#[cfg(any(
    feature = "int-sqlite",
    feature = "int-postgres",
    feature = "int-mysql"
))]
pub(crate) mod sql;
#[cfg(feature = "int-sqlite")]
pub mod sqlite;
#[cfg(feature = "int-ssh")]
pub mod ssh;
#[cfg(feature = "int-telegram")]
pub mod telegram;
#[cfg(any(feature = "int-postgres", feature = "int-redis", feature = "int-kafka"))]
pub(crate) mod tls;

pub use credentials::{
    AppliedAuth, Authenticate, CredentialError, CredentialType, CredentialTypes, builtin_types,
};
pub use poll::{PollRegistry, PollingHost, PollingTrigger};
#[cfg(feature = "int-searxng")]
pub use searxng::{searxng_web_search_tool, searxng_web_search_tool_with_token};

use hk_flow::{FlowError, NodeRegistry};

/// Register the generic credential types and every integration enabled by cargo
/// features. Polling triggers are not included (they need a host): use
/// [`register_integrations_with_polling`].
pub fn register_integrations(
    nodes: &mut NodeRegistry,
    creds: &CredentialTypes,
) -> Result<(), FlowError> {
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
    #[cfg(feature = "int-sqlite")]
    sqlite::register(nodes, creds)?;
    #[cfg(feature = "int-postgres")]
    postgres::register(nodes, creds)?;
    #[cfg(feature = "int-redis")]
    redis::register(nodes, creds)?;
    #[cfg(feature = "int-s3")]
    s3::register(nodes, creds)?;
    #[cfg(feature = "int-searxng")]
    searxng::register(nodes, creds)?;
    #[cfg(feature = "int-ollama")]
    ollama::register(nodes, creds)?;
    #[cfg(feature = "int-slack")]
    slack::register(nodes, creds)?;
    #[cfg(feature = "int-mysql")]
    mysql::register(nodes, creds)?;
    #[cfg(feature = "int-mqtt")]
    mqtt::register(nodes, creds)?;
    #[cfg(feature = "int-rabbitmq")]
    rabbitmq::register(nodes, creds)?;
    #[cfg(feature = "int-ssh")]
    ssh::register(nodes, creds)?;
    #[cfg(feature = "int-ftp")]
    ftp::register(nodes, creds)?;
    #[cfg(feature = "int-kafka")]
    kafka::register(nodes, creds)?;
    #[cfg(all(feature = "int-mongodb", not(target_os = "android")))]
    mongodb::register(nodes, creds)?;
    #[cfg(feature = "int-saas")]
    declarative::register(nodes, creds)?;
    Ok(())
}
