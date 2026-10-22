//! Plugin tools in the agent's tool registry.

use std::sync::Arc;

use async_trait::async_trait;
use hk_tools::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};
use serde_json::Value;

use crate::host::LoadedPlugin;

/// One tool of a plugin. The plugin runs in a sandbox, but a tool that declares `ask` or
/// `dangerous` still goes through the user's approval policy like any other tool.
#[derive(Debug)]
struct PluginTool {
    plugin: Arc<LoadedPlugin>,
    name: String,
    description: String,
    parameters: Value,
    risk: Risk,
}

#[async_trait]
impl Tool for PluginTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec { name: self.name.clone(), description: format!("[plugin {}] {}", self.plugin.manifest.name, self.description), parameters: self.parameters.clone() }
    }

    fn risk(&self, _args: &Value) -> Risk {
        self.risk
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let (plugin, name, text) = (self.plugin.clone(), self.name.clone(), args.to_string());
        let res = tokio::task::spawn_blocking(move || plugin.call_tool(&name, &text)).await.map_err(|e| ToolError::Failed { tool: self.name.clone(), reason: e.to_string() })?;
        match res {
            Ok(text) => Ok(ToolOutput::untrusted(&format!("plugin:{}", self.name), &text)),
            // A plugin's own error is a normal tool error the model can react to.
            Err(crate::PluginError::Plugin(msg)) => Ok(ToolOutput::untrusted_error(&format!("plugin:{}", self.name), &msg)),
            Err(e) => Err(ToolError::Failed { tool: self.name.clone(), reason: e.to_string() }),
        }
    }
}

/// The plugin's tools, for `AgentRuntime::add_tools`.
pub fn plugin_tools(plugin: &Arc<LoadedPlugin>) -> Vec<Arc<dyn Tool>> {
    plugin
        .description
        .tools
        .iter()
        .map(|t| {
            let risk = match t.risk.as_str() {
                "dangerous" => Risk::Dangerous,
                "safe" => Risk::Safe,
                // Unknown or missing: ask.
                _ => Risk::Ask,
            };
            Arc::new(PluginTool {
                plugin: plugin.clone(),
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: if t.parameters.is_null() { serde_json::json!({"type": "object"}) } else { t.parameters.clone() },
                risk,
            }) as Arc<dyn Tool>
        })
        .collect()
}
