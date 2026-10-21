use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::McpError;

/// One configured MCP server. `name` becomes the namespace segment in `mcp.<name>.<tool>`.
#[derive(Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(flatten)]
    pub transport: McpTransportConfig,
    /// Per-request timeout (tool calls, listings). Default 120s.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_timeout_secs() -> u64 {
    120
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpTransportConfig {
    Stdio(StdioConfig),
    Http(HttpConfig),
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct StdioConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra env for the child. Values are resolved secrets; the host env is NOT inherited
    /// beyond a small allowlist (PATH, HOME, ...).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct HttpConfig {
    pub url: String,
    /// Bearer token, without the `Bearer ` prefix. Resolved from `hk-store::secrets` by caller.
    #[serde(default)]
    pub bearer_token: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl McpServerConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.max(1))
    }

    /// Server name must be a safe namespace segment: it is embedded in tool names.
    pub fn validate(&self) -> Result<(), McpError> {
        let ok = !self.name.is_empty()
            && self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !ok {
            return Err(McpError::Config(format!(
                "server name `{}` must match [A-Za-z0-9_-]+",
                self.name
            )));
        }
        match &self.transport {
            McpTransportConfig::Stdio(s) if s.command.trim().is_empty() => {
                Err(McpError::Config("stdio command is empty".into()))
            }
            McpTransportConfig::Http(h) if h.url.trim().is_empty() => {
                Err(McpError::Config("http url is empty".into()))
            }
            _ => Ok(()),
        }
    }
}

// Manual Debug: env values, tokens and header values are secrets and must never reach logs.
impl fmt::Debug for McpServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServerConfig")
            .field("name", &self.name)
            .field("transport", &self.transport)
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

impl fmt::Debug for McpTransportConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdio(s) => s.fmt(f),
            Self::Http(h) => h.fmt(f),
        }
    }
}

impl fmt::Debug for StdioConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StdioConfig")
            .field("command", &self.command)
            .field("args", &self.args)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field("cwd", &self.cwd)
            .finish()
    }
}

impl fmt::Debug for HttpConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpConfig")
            .field("url", &self.url)
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "<redacted>"),
            )
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}
