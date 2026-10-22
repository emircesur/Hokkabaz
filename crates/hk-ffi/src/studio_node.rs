//! Flow node `hokkabaz.studio`: run a Video Studio operation from a flow (transcribe a clip,
//! find highlights, render, publish ...). Each operation is the agent's `studio_*` tool, so a
//! flow and the agent do exactly the same thing. Needs the in-process core (ffmpeg lives there).

use async_trait::async_trait;
use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};
use hk_tools::{ToolCtx, ToolError};
use serde_json::{Map, Value, json};

pub const NODE_TYPE: &str = "hokkabaz.studio";

const OPS: &[(&str, &str, &str)] = &[
    (
        "studio_projects",
        "List or create projects",
        "{\"action\": \"list\"}",
    ),
    (
        "studio_transcribe",
        "Transcribe a project",
        "{\"project\": \"\"}",
    ),
    (
        "studio_highlights",
        "Find highlight clips",
        "{\"project\": \"\"}",
    ),
    (
        "studio_edit",
        "Apply edit operations",
        "{\"project\": \"\", \"ops\": []}",
    ),
    (
        "studio_reframe",
        "Plan 9:16 reframing",
        "{\"project\": \"\"}",
    ),
    (
        "studio_broll",
        "Suggest or add B-roll",
        "{\"project\": \"\"}",
    ),
    ("studio_social", "Draft social copy", "{\"project\": \"\"}"),
    ("studio_render", "Render the project", "{\"project\": \"\"}"),
    ("studio_export", "Export the project", "{\"project\": \"\"}"),
    (
        "studio_publish",
        "Publish a rendered clip",
        "{\"project\": \"\"}",
    ),
];

fn description() -> NodeTypeDescription {
    serde_json::from_value(json!({
        "displayName": "Video Studio",
        "name": NODE_TYPE,
        "version": [1],
        "description": "Run a Video Studio step (this device only)",
        "group": ["transform"],
        "defaults": {"name": "Video Studio", "color": "#8B7CF6"},
        "inputs": ["main"],
        "outputs": ["main"],
        "properties": [
            {"displayName": "Operation", "name": "operation", "type": "options", "default": "studio_transcribe",
             "options": OPS.iter().map(|(v, n, _)| json!({"name": n, "value": v})).collect::<Vec<_>>()},
            {"displayName": "Arguments", "name": "arguments", "type": "json", "default": "{\"project\": \"\"}",
             "description": "JSON passed to the step; see the Studio guide. Expressions work per item."}
        ]
    }))
    .expect("static studio node description")
}

#[derive(Debug)]
struct StudioNode(NodeTypeDescription);

fn err(ctx: &NodeContext, m: impl Into<String>) -> FlowError {
    FlowError::Node {
        node: ctx.node.name.clone(),
        message: m.into(),
    }
}

#[async_trait]
impl FlowNode for StudioNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let svc = crate::studio::service()
            .map_err(|e| err(ctx, format!("Video Studio is not available here: {e}")))?;
        let tools = hk_studio::studio_tools(svc);
        let mut out = Vec::new();
        for (i, _) in ctx.input.first().into_iter().flatten().enumerate() {
            let op = match ctx.eval_param_at("operation", i)? {
                Some(Value::String(s)) => s,
                _ => "studio_transcribe".to_owned(),
            };
            if !OPS.iter().any(|(v, _, _)| *v == op) {
                return Err(err(ctx, format!("Unknown Studio operation `{op}`")));
            }
            let tool = tools
                .iter()
                .find(|t| t.spec().name == op)
                .ok_or_else(|| err(ctx, format!("`{op}` is not available")))?;
            let args = match ctx.eval_param_at("arguments", i)? {
                Some(Value::String(s)) if s.trim().is_empty() => Value::Object(Map::new()),
                Some(Value::String(s)) => serde_json::from_str(&s)
                    .map_err(|e| err(ctx, format!("Arguments is not valid JSON: {e}")))?,
                Some(v) => v,
                None => Value::Object(Map::new()),
            };
            let res = tool.execute(ToolCtx::default(), args).await;
            let (content, is_error) = match res {
                Ok(o) => (o.content, o.is_error),
                Err(ToolError::InvalidArgs { reason, .. }) => {
                    return Err(err(ctx, format!("Invalid arguments: {reason}")));
                }
                Err(e) => return Err(err(ctx, e.to_string())),
            };
            if is_error {
                return Err(err(ctx, content));
            }
            let result = serde_json::from_str::<Value>(&content).unwrap_or(Value::String(content));
            let mut item = Item::from_json(json!({"operation": op, "result": result}));
            item.paired_item = Some(hk_flow::PairedItem::Index(i));
            out.push(item);
        }
        Ok(vec![out])
    }
}

pub fn register(reg: &mut NodeRegistry) {
    if let Err(e) = reg.register(std::sync::Arc::new(StudioNode(description()))) {
        tracing::warn!(error = %e, "studio node not registered");
    }
}
