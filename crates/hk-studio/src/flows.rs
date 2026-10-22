use std::sync::Arc;

use async_trait::async_trait;
use hk_flow::{
    FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry, NodeTypeDescription,
};
use serde_json::{Value, json};

use crate::Studio;

/// `None` registers descriptions for the canvas; execution requires an attached Studio.
pub fn register_studio_nodes(
    registry: &mut NodeRegistry,
    studio: Option<Arc<Studio>>,
) -> std::result::Result<(), FlowError> {
    for (kind, name, operation) in [
        ("Import", "Import video", "import"),
        ("Transcribe", "Transcribe video", "transcribe"),
        ("Highlights", "Find highlights", "analyze"),
        ("Reframe", "Reframe video", "reframe"),
        ("Score", "Score clip", "score_clip"),
        ("Caption", "Style captions", "template"),
        ("Broll", "Add B-roll", "add_broll"),
        ("Render", "Render video", "render"),
        ("Jobs", "Render status", "jobs"),
        ("Social", "Write social metadata", "social"),
        ("Export", "Export video package", "export_package"),
        ("Publish", "Upload to YouTube", "publish"),
    ] {
        let description: NodeTypeDescription=serde_json::from_value(json!({
            "displayName":name,"name":format!("hokkabaz.studio{kind}"),"version":1,
            "description":format!("Hokkabaz Studio: {name}. Uses connections configured on this instance."),
            "group":["transform"],"defaults":{"name":name,"color":"#8B7CF6"},"inputs":["main"],"outputs":["main"],
            "properties":[{"displayName":"Project ID","name":"project_id","type":"string","default":"={{ $json.project_id }}"},
                {"displayName":"Arguments","name":"arguments","type":"json","default":"{}","description":"Operation arguments. Publishing requires confirmed:true and a completed job_id; default visibility is private."}]
        })).map_err(|e|FlowError::Node { node:name.into(),message:e.to_string() })?;
        registry.register(Arc::new(StudioNode {
            description,
            operation,
            studio: studio.clone(),
        }))?;
    }
    Ok(())
}

#[derive(Debug)]
struct StudioNode {
    description: NodeTypeDescription,
    operation: &'static str,
    studio: Option<Arc<Studio>>,
}

#[async_trait]
impl FlowNode for StudioNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.description
    }

    async fn execute(&self, ctx: &NodeContext) -> std::result::Result<NodeOutput, FlowError> {
        let error = |message: String| FlowError::Node {
            node: ctx.node.name.clone(),
            message,
        };
        let studio = self
            .studio
            .as_ref()
            .ok_or_else(|| error("Studio unavailable on this instance".into()))?;
        let input = ctx.input.first().map(Vec::as_slice).unwrap_or(&[]);
        let mut output = Vec::new();
        for (index, item) in input.iter().enumerate() {
            let mut args = item.json.clone();
            if let Some(project) = ctx.eval_param_at("project_id", index)? {
                args.insert("project_id".into(), project);
            }
            if let Some(extra) = ctx.eval_param_at("arguments", index)? {
                let extra = match extra {
                    Value::String(s) => serde_json::from_str(&s)
                        .map_err(|e| error(format!("arguments JSON: {e}")))?,
                    v => v,
                };
                let Value::Object(extra) = extra else {
                    return Err(error("arguments must be a JSON object".into()));
                };
                args.extend(extra);
            }
            let result = studio
                .request(self.operation, Value::Object(args.clone()))
                .await
                .map_err(|e| error(e.to_string()))?;
            if let Some(id) = result.pointer("/project/id") {
                args.insert("project_id".into(), id.clone());
            }
            args.insert("studio".into(), result);
            output.push(Item::new(args));
        }
        Ok(vec![output])
    }
}
