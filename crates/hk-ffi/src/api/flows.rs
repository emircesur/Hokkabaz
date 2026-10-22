//! `flows::*` surface: node-type catalog + workflow validation for the canvas.
//!
//! Stateless on purpose: workflows are plain n8n JSON owned by Dart for now (hk-store / REST
//! persistence lands later), so nothing here touches the core handle.

use flutter_rust_bridge::frb;
use hk_flow::{IoDecl, NodeRegistry, NodeTypeDescription, Workflow};

use crate::dto::NodeTypeDto;
use crate::error::HkError;

fn registry() -> Result<std::sync::Arc<NodeRegistry>, HkError> {
    Ok(crate::nodes::registry())
}

fn io_types(decl: &IoDecl) -> (Vec<String>, Option<String>) {
    match decl {
        IoDecl::Expression(e) => (Vec::new(), Some(e.clone())),
        IoDecl::List(l) => (l.iter().map(|s| s.conn_type().to_owned()).collect(), None),
    }
}

fn to_dto(d: &NodeTypeDescription) -> Result<NodeTypeDto, HkError> {
    let mut versions: Vec<f64> = d.version.to_vec().into_iter().map(|v| v.get()).collect();
    versions.sort_by(f64::total_cmp);
    let default_version = d
        .latest_version()
        .map(|v| v.get())
        .or_else(|| versions.last().copied())
        .unwrap_or(1.0);
    let (inputs, inputs_expr) = io_types(&d.inputs);
    let (outputs, outputs_expr) = io_types(&d.outputs);
    Ok(NodeTypeDto {
        type_name: d.name.clone(),
        versions,
        default_version,
        display_name: d.display_name.clone(),
        description: d.description.clone(),
        group: d.group.clone(),
        inputs,
        outputs,
        inputs_expr,
        outputs_expr,
        output_names: d.output_names.clone().unwrap_or_default(),
        properties_json: serde_json::to_string(&d.properties)?,
        credentials_json: serde_json::to_string(&d.credentials)?,
    })
}

/// Every node type the engine can run, sorted by display name. Types missing here are shown as
/// unsupported placeholders on the canvas (JSON stays intact).
#[frb(sync)]
pub fn list_node_types() -> Result<Vec<NodeTypeDto>, HkError> {
    let reg = registry()?;
    let mut out = Vec::new();
    for ty in reg.types() {
        if let Some(n) = reg.latest(ty) {
            let d = n.description();
            if d.hidden.unwrap_or(false) {
                continue;
            }
            out.push(to_dto(d)?);
        }
    }
    out.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
            .then_with(|| a.type_name.cmp(&b.type_name))
    });
    Ok(out)
}

/// Structural problems (invalid JSON, duplicate node names, dangling connections) as errors,
/// unsupported node types as `warning: ...`. Empty = clean.
#[frb(sync)]
pub fn validate_workflow(json: String) -> Result<Vec<String>, HkError> {
    let wf = match Workflow::from_json_str(&json) {
        Ok(w) => w,
        Err(e) => return Ok(vec![format!("error: {e}")]),
    };
    let mut out = Vec::new();
    if let Err(e) = wf.validate() {
        out.push(format!("error: {e}"));
    }
    let reg = registry()?;
    for n in &wf.nodes {
        if !reg.is_supported(n) {
            out.push(format!(
                "warning: unsupported node `{}` ({}@{})",
                n.name,
                n.node_type,
                n.version()
            ));
        }
    }
    Ok(out)
}
