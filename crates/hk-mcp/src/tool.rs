use std::sync::Arc;

use async_trait::async_trait;
use hk_tools::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};
use serde_json::Value;

use crate::client::McpClient;
use crate::convert::ToolInfo;

/// Registry name for a server tool: `mcp.<server>.<tool>` (DESIGN §3.7).
pub fn qualified_name(server: &str, tool: &str) -> String {
    format!("mcp.{server}.{tool}")
}

/// One remote MCP tool exposed through the hk-tools `Tool` trait.
pub struct McpTool {
    client: Arc<McpClient>,
    info: ToolInfo,
    qualified: String,
}

impl std::fmt::Debug for McpTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpTool")
            .field("name", &self.qualified)
            .finish_non_exhaustive()
    }
}

impl McpTool {
    pub fn new(client: Arc<McpClient>, info: ToolInfo) -> Self {
        let qualified = qualified_name(client.name(), &info.name);
        Self {
            client,
            info,
            qualified,
        }
    }

    pub(crate) fn permissive(mut self) -> Self {
        self.info.input_schema = serde_json::json!({"type": "object"});
        self
    }
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        let schema = if self.info.input_schema.is_object() {
            self.info.input_schema.clone()
        } else {
            serde_json::json!({"type": "object"})
        };
        ToolSpec {
            name: self.qualified.clone(),
            description: self.info.description.clone().unwrap_or_default(),
            parameters: schema,
        }
    }

    /// Remote code we do not control: always ask. Server-supplied annotations are hints only.
    fn risk(&self, _args: &Value) -> Risk {
        Risk::Ask
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let args = match args {
            Value::Object(m) => Some(m),
            Value::Null => None,
            _ => {
                return Err(ToolError::InvalidArgs {
                    tool: self.qualified.clone(),
                    reason: "arguments must be an object".into(),
                });
            }
        };
        let (text, is_error) = self
            .client
            .call_tool(&self.info.name, args)
            .await
            .map_err(|e| ToolError::Failed {
                tool: self.qualified.clone(),
                reason: e.to_string(),
            })?;
        Ok(if is_error {
            ToolOutput::error(text)
        } else {
            ToolOutput::ok(text)
        })
    }
}
