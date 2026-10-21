use std::future::Future;
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, GetPromptRequestParams, ReadResourceRequestParams};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Map, Value};

use crate::config::{McpServerConfig, McpTransportConfig};
use crate::convert::{
    PromptInfo, PromptMessageInfo, ResourceInfo, ToolInfo, call_result_to_text, list_via_json,
    prompt_messages, resource_contents_text, via_json,
};
use crate::error::McpError;

/// Live connection to one MCP server. Dropping it closes the transport (and kills a stdio child).
pub struct McpClient {
    name: String,
    timeout: Duration,
    service: RunningService<RoleClient, ()>,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl McpClient {
    /// Spawn/connect and run the MCP `initialize` handshake.
    pub async fn connect(cfg: &McpServerConfig) -> Result<Self, McpError> {
        cfg.validate()?;
        let timeout = cfg.timeout();
        let service = tokio::time::timeout(timeout, connect_transport(cfg))
            .await
            .map_err(|_| McpError::Timeout(timeout))??;
        Ok(Self {
            name: cfg.name.clone(),
            timeout,
            service,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>, McpError> {
        let tools = self.with_timeout(self.service.list_all_tools()).await?;
        list_via_json(&tools)
    }

    pub async fn list_resources(&self) -> Result<Vec<ResourceInfo>, McpError> {
        let r = self.with_timeout(self.service.list_all_resources()).await?;
        list_via_json(&r)
    }

    pub async fn list_prompts(&self) -> Result<Vec<PromptInfo>, McpError> {
        let p = self.with_timeout(self.service.list_all_prompts()).await?;
        list_via_json(&p)
    }

    /// Text of a resource (binary contents are described, not inlined).
    pub async fn read_resource(&self, uri: &str) -> Result<String, McpError> {
        let r = self
            .with_timeout(
                self.service
                    .read_resource(ReadResourceRequestParams::new(uri)),
            )
            .await?;
        Ok(resource_contents_text(&via_json::<_, Value>(&r)?))
    }

    pub async fn get_prompt(
        &self,
        name: &str,
        args: Option<Map<String, Value>>,
    ) -> Result<Vec<PromptMessageInfo>, McpError> {
        let mut params = GetPromptRequestParams::new(name);
        if let Some(a) = args {
            params = params.with_arguments(a);
        }
        let r = self.with_timeout(self.service.get_prompt(params)).await?;
        Ok(prompt_messages(&via_json::<_, Value>(&r)?))
    }

    /// Call a server tool by its *unqualified* name. Returns `(text, is_error)`.
    pub async fn call_tool(
        &self,
        tool: &str,
        args: Option<Map<String, Value>>,
    ) -> Result<(String, bool), McpError> {
        let mut params = CallToolRequestParams::new(tool.to_owned());
        if let Some(a) = args {
            params = params.with_arguments(a);
        }
        let r = self.with_timeout(self.service.call_tool(params)).await?;
        Ok(call_result_to_text(&via_json::<_, Value>(&r)?))
    }

    async fn with_timeout<T, E: std::fmt::Display>(
        &self,
        fut: impl Future<Output = Result<T, E>>,
    ) -> Result<T, McpError> {
        match tokio::time::timeout(self.timeout, fut).await {
            Err(_) => Err(McpError::Timeout(self.timeout)),
            Ok(Err(e)) => Err(McpError::Request(e.to_string())),
            Ok(Ok(v)) => Ok(v),
        }
    }
}

async fn connect_transport(
    cfg: &McpServerConfig,
) -> Result<RunningService<RoleClient, ()>, McpError> {
    match &cfg.transport {
        McpTransportConfig::Stdio(s) => connect_stdio(s).await,
        McpTransportConfig::Http(h) => connect_http(h).await,
    }
}

/// Env vars a child may inherit. Everything else (API keys etc.) must be passed explicitly.
#[cfg(all(feature = "stdio", not(target_os = "android")))]
const INHERIT_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SYSTEMROOT",
    "USERPROFILE",
    "APPDATA",
    "SHELL",
];

#[cfg(all(feature = "stdio", not(target_os = "android")))]
async fn connect_stdio(
    s: &crate::config::StdioConfig,
) -> Result<RunningService<RoleClient, ()>, McpError> {
    use std::process::Stdio;

    use rmcp::ServiceExt;

    let mut cmd = tokio::process::Command::new(&s.command);
    cmd.args(&s.args).env_clear();
    for k in INHERIT_ENV {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    cmd.envs(&s.env);
    if let Some(cwd) = &s.cwd {
        cmd.current_dir(cwd);
    }
    // stderr dropped: server logs are noise here and may echo secrets.
    let (transport, _) = rmcp::transport::TokioChildProcess::builder(cmd)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| McpError::Connect(format!("spawn `{}`: {e}", s.command)))?;
    ().serve(transport)
        .await
        .map_err(|e| McpError::Connect(e.to_string()))
}

#[cfg(not(all(feature = "stdio", not(target_os = "android"))))]
async fn connect_stdio(
    _s: &crate::config::StdioConfig,
) -> Result<RunningService<RoleClient, ()>, McpError> {
    Err(McpError::Unsupported(
        "stdio MCP servers need the `stdio` feature and a non-android target",
    ))
}

#[cfg(feature = "http")]
async fn connect_http(
    h: &crate::config::HttpConfig,
) -> Result<RunningService<RoleClient, ()>, McpError> {
    use http::{HeaderName, HeaderValue};
    use rmcp::ServiceExt;
    use rmcp::transport::StreamableHttpClientTransport;
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;

    let mut headers = std::collections::HashMap::new();
    for (k, v) in &h.headers {
        // Error text names the header, never the value.
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|_| McpError::Config(format!("invalid header name `{k}`")))?;
        let mut val = HeaderValue::from_str(v)
            .map_err(|_| McpError::Config(format!("invalid value for header `{k}`")))?;
        val.set_sensitive(true);
        headers.insert(name, val);
    }
    let mut config =
        StreamableHttpClientTransportConfig::with_uri(h.url.as_str()).custom_headers(headers);
    if let Some(t) = &h.bearer_token {
        config = config.auth_header(t.as_str());
    }
    let transport = StreamableHttpClientTransport::from_config(config);
    ().serve(transport)
        .await
        .map_err(|e| McpError::Connect(e.to_string()))
}

#[cfg(not(feature = "http"))]
async fn connect_http(
    _h: &crate::config::HttpConfig,
) -> Result<RunningService<RoleClient, ()>, McpError> {
    Err(McpError::Unsupported(
        "http MCP servers need the `http` feature",
    ))
}
