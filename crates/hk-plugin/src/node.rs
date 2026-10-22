//! Plugin nodes in the flow engine's node registry.

use std::sync::Arc;

use async_trait::async_trait;
use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};
use serde_json::{Value, json};

use crate::error::PluginError;
use crate::host::LoadedPlugin;

#[derive(Debug)]
struct PluginNode {
    plugin: Arc<LoadedPlugin>,
    description: NodeTypeDescription,
}

fn node_err(ctx: &NodeContext, message: impl Into<String>) -> FlowError {
    FlowError::Node { node: ctx.node.name.clone(), message: message.into() }
}

#[async_trait]
impl FlowNode for PluginNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.description
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let items: Vec<Item> = ctx.input.first().cloned().unwrap_or_default();
        // Parameters are evaluated against the first item (expressions like {{ $json.x }} work
        // per run, not per item); plugins that need per-item values read them from the items.
        let mut params = serde_json::Map::new();
        for (k, v) in &ctx.node.parameters {
            params.insert(k.clone(), ctx.eval_param(v, 0).map_err(|e| node_err(ctx, e.to_string()))?);
        }
        let input = json!({
            "params": Value::Object(params),
            "items": items.iter().map(|i| json!({"json": i.json})).collect::<Vec<_>>(),
        });
        let (plugin, ty) = (self.plugin.clone(), self.description.name.clone());
        let out = tokio::task::spawn_blocking(move || plugin.execute_node(&ty, input))
            .await
            .map_err(|e| node_err(ctx, e.to_string()))?
            .map_err(|e| match e {
                PluginError::Plugin(m) => node_err(ctx, m),
                other => node_err(ctx, other.to_string()),
            })?;
        let outputs = out["items"].as_array().ok_or_else(|| node_err(ctx, "plugin returned no `items` outputs"))?;
        Ok(outputs
            .iter()
            .map(|port| {
                port.as_array()
                    .map(|a| {
                        a.iter()
                            .enumerate()
                            .map(|(i, v)| {
                                let mut item = Item::default();
                                if let Some(obj) = v.get("json").and_then(Value::as_object) {
                                    item.json = obj.clone();
                                } else if let Some(obj) = v.as_object() {
                                    item.json = obj.clone();
                                }
                                // Keep the link to the input item when counts line up.
                                if a.len() == items.len() {
                                    item.paired_item = Some(hk_flow::PairedItem::Index(i));
                                }
                                item
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect())
    }
}

/// Register the plugin's nodes. Returns how many were registered; nodes whose description does
/// not parse are skipped with a warning (the rest of the plugin still works).
pub fn register_plugin_nodes(registry: &mut NodeRegistry, plugin: &Arc<LoadedPlugin>) -> usize {
    let mut n = 0;
    for raw in &plugin.description.nodes {
        match serde_json::from_value::<NodeTypeDescription>(raw.clone()) {
            Ok(description) => {
                let node: Arc<dyn FlowNode> = Arc::new(PluginNode { plugin: plugin.clone(), description });
                match registry.register(node) {
                    Ok(()) => n += 1,
                    Err(e) => tracing::warn!(plugin = %plugin.manifest.id, error = %e, "plugin node not registered"),
                }
            }
            Err(e) => tracing::warn!(plugin = %plugin.manifest.id, error = %e, "plugin node description is invalid"),
        }
    }
    n
}
