//! Link to the user's n8n instance (see `n8n_sync`). Needs the in-process core.

use serde_json::{Value, json};

use crate::error::HkError;
use crate::n8n_sync::get;

fn to_json(v: &Value) -> Result<String, HkError> {
    serde_json::to_string(v).map_err(|e| HkError::internal(e.to_string()))
}

/// `{credential_id, delegate}`.
pub async fn n8n_status() -> Result<String, HkError> {
    let l = get()?.link();
    to_json(&json!({"credential_id": l.credential_id, "delegate": l.delegate}))
}

/// Pick the `n8nApi` credential for the instance (None unlinks) and whether unsupported node
/// types run there.
pub async fn n8n_configure(credential_id: Option<String>, delegate: bool) -> Result<(), HkError> {
    get()?.configure(credential_id, delegate).await
}

pub async fn n8n_test() -> Result<(), HkError> {
    get()?.test().await
}

/// `[{id, name, active, updated_at, nodes}]`.
pub async fn n8n_list_workflows() -> Result<String, HkError> {
    to_json(&get()?.list_workflows().await?)
}

/// `{workflow, unmapped: [{node, type, name}]}`: the workflow JSON plus credentials with no local match.
pub async fn n8n_pull(id: String) -> Result<String, HkError> {
    to_json(&get()?.pull(&id).await?)
}

/// Create the workflow on the instance, or replace `remote_id`. Returns the remote id.
pub async fn n8n_push(workflow_json: String, remote_id: Option<String>) -> Result<String, HkError> {
    let wf: Value = serde_json::from_str(&workflow_json)
        .map_err(|e| HkError::invalid(format!("workflow JSON: {e}")))?;
    get()?.push(&wf, remote_id.as_deref()).await
}
