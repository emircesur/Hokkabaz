use std::sync::Arc;

use hk_core::Effect;
use hk_tools::{ToolError, ToolRegistry};

use crate::client::McpClient;
use crate::config::McpServerConfig;
use crate::error::McpError;
use crate::tool::McpTool;

/// A connected server plus its registered tools. Dropping unregisters the tools (via `Effect`)
/// and closes the connection.
#[derive(Debug)]
pub struct McpServer {
    client: Arc<McpClient>,
    // Held for Drop: each unregisters one tool.
    _effects: Vec<Effect>,
    tool_names: Vec<String>,
}

impl McpServer {
    /// Connect, list tools, register each as `mcp.<server>.<tool>`.
    pub async fn connect(cfg: &McpServerConfig, registry: &ToolRegistry) -> Result<Self, McpError> {
        let client = Arc::new(McpClient::connect(cfg).await?);
        let infos = client.list_tools().await?;
        let mut effects = Vec::with_capacity(infos.len());
        let mut tool_names = Vec::with_capacity(infos.len());
        for info in infos {
            let tool = McpTool::new(client.clone(), info.clone());
            let name = hk_tools::Tool::spec(&tool).name;
            let effect = match registry.register(Arc::new(tool)) {
                Ok(e) => e,
                Err(ToolError::BadSchema { tool: t, reason }) => {
                    // Servers ship sloppy schemas; one bad tool must not block the rest.
                    tracing::warn!(tool = %t, %reason, "mcp tool schema invalid, using permissive schema");
                    registry.register(Arc::new(McpTool::new(client.clone(), info).permissive()))?
                }
                Err(e) => return Err(e.into()),
            };
            effects.push(effect);
            tool_names.push(name);
        }
        Ok(Self {
            client,
            _effects: effects,
            tool_names,
        })
    }

    pub fn client(&self) -> &Arc<McpClient> {
        &self.client
    }

    /// Qualified names registered for this server.
    pub fn tool_names(&self) -> &[String] {
        &self.tool_names
    }
}
