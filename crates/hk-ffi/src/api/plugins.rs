//! Plugins (WASM) and MCP servers. Needs the in-process core: both extend what the local agent
//! and flow engine can do. Structured values cross as JSON strings.

use std::path::Path;

use serde_json::Value;

use crate::error::HkError;
use crate::plugins::manager;

fn to_json(v: &Value) -> Result<String, HkError> {
    serde_json::to_string(v).map_err(|e| HkError::internal(e.to_string()))
}

/// Installed plugins: id, name, version, enabled, loaded, capabilities (human-readable lines),
/// error, tools/nodes/triggers, settings_schema + settings.
pub async fn plugin_list() -> Result<String, HkError> {
    to_json(&manager()?.list_json()?)
}

/// Install a `.tar.gz` plugin archive from disk. Returns the plugin id.
pub async fn plugin_install_file(path: String) -> Result<String, HkError> {
    let bytes = std::fs::read(&path).map_err(|e| HkError::invalid(format!("{path}: {e}")))?;
    manager()?.install_archive(&bytes)
}

/// Install from a directory holding `plugin.toml` + the wasm (what `hk plugin build` writes).
pub async fn plugin_install_dir(dir: String) -> Result<String, HkError> {
    manager()?.install_dir(Path::new(&dir))
}

/// Registry listing with install/update state. `index_url` defaults to the official registry.
pub async fn plugin_registry(index_url: Option<String>) -> Result<String, HkError> {
    to_json(&manager()?.registry_json(index_url.as_deref()).await?)
}

pub async fn plugin_install_from_registry(id: String, index_url: Option<String>) -> Result<String, HkError> {
    manager()?.install_from_registry(&id, index_url.as_deref()).await
}

pub async fn plugin_remove(id: String) -> Result<(), HkError> {
    manager()?.remove(&id)
}

pub async fn plugin_set_enabled(id: String, enabled: bool) -> Result<(), HkError> {
    manager()?.set_enabled(&id, enabled)
}

/// `settings_json` is the plugin's whole settings object (validated by the plugin on use).
pub async fn plugin_set_settings(id: String, settings_json: String) -> Result<(), HkError> {
    let v: Value = serde_json::from_str(&settings_json).map_err(|e| HkError::invalid(format!("settings JSON: {e}")))?;
    manager()?.set_settings(&id, &v)
}

/// MCP servers with trust and connection state (no secrets).
pub async fn mcp_list() -> Result<String, HkError> {
    to_json(&manager()?.mcp_json().await)
}

/// Add or replace a server. Spec: `{name, transport:"stdio"|"http", command, args[], env{}, cwd,
/// url, bearer_token, headers{}, timeout_secs}`; secrets go to the secret store and any change
/// resets trust.
pub async fn mcp_save(spec_json: String) -> Result<(), HkError> {
    let v: Value = serde_json::from_str(&spec_json).map_err(|e| HkError::invalid(format!("MCP spec JSON: {e}")))?;
    manager()?.mcp_save(&v).await
}

/// A server's tools run only after the user trusted it.
pub async fn mcp_set_trusted(name: String, trusted: bool) -> Result<(), HkError> {
    manager()?.mcp_set_trusted(&name, trusted).await
}

pub async fn mcp_set_enabled(name: String, enabled: bool) -> Result<(), HkError> {
    manager()?.mcp_set_enabled(&name, enabled).await
}

pub async fn mcp_remove(name: String) -> Result<(), HkError> {
    manager()?.mcp_remove(&name).await
}

pub async fn mcp_connect(name: String) -> Result<(), HkError> {
    manager()?.mcp_connect(&name).await
}
