//! REST v1 mirroring the n8n public API: workflows, executions, credentials.
//!
//! Shapes follow the public docs (`{data, nextCursor}` pages, camelCase fields, ISO-8601
//! timestamps). Credentials are write-only: values go straight into the `SecretStore` under
//! `credential.<id>` and no route returns them.

use axum::extract::{Path, Query, State};
use axum::Json;
use hk_store::{CredentialRow, ExecutionFilter, ExecutionRow, Secret, WorkflowRow};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use utoipa::IntoParams;

use crate::api::AppState;
use crate::auth::{Principal, Scope};
use crate::error::ApiError;
use crate::flow::FlowApi;

const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 250;
const MAX_NAME: usize = 128;

pub(crate) fn flow(s: &AppState) -> Result<&FlowApi, ApiError> {
    s.flow.as_ref().ok_or_else(|| ApiError::NotFound("flow API not enabled".into()))
}

/// Unix ms -> `2026-01-02T03:04:05.678Z` (civil-from-days, no chrono dependency).
pub(crate) fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms.rem_euclid(1000)
    )
}

fn random_bytes<const N: usize>() -> Result<[u8; N], ApiError> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|_| ApiError::Internal("random source unavailable".into()))?;
    Ok(b)
}

/// 16 chars like n8n's workflow ids.
pub(crate) fn new_id() -> Result<String, ApiError> {
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    Ok(random_bytes::<16>()?.iter().map(|b| A[*b as usize % A.len()] as char).collect())
}

pub(crate) fn new_version_id() -> Result<String, ApiError> {
    let b = random_bytes::<16>()?;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
}

// ------------------------------------------------------------------ workflows

/// Fields owned by the server; clients can't set them.
const RESERVED: [&str; 6] = ["id", "name", "active", "createdAt", "updatedAt", "versionId"];

pub(crate) struct Prepared {
    pub(crate) name: String,
    pub(crate) data: Map<String, Value>,
}

pub(crate) fn prepare_workflow(body: Value) -> Result<Prepared, ApiError> {
    let Value::Object(mut m) = body else {
        return Err(ApiError::BadRequest("request body must be a JSON object".into()));
    };
    let name = match m.get("name") {
        Some(Value::String(n)) if !n.trim().is_empty() && n.chars().count() <= MAX_NAME => n.clone(),
        _ => return Err(ApiError::BadRequest(format!("`name` must be a non-empty string up to {MAX_NAME} chars"))),
    };
    if !m.get("nodes").is_some_and(Value::is_array) {
        return Err(ApiError::BadRequest("`nodes` must be an array".into()));
    }
    if !m.get("connections").is_some_and(Value::is_object) {
        return Err(ApiError::BadRequest("`connections` must be an object".into()));
    }
    if m.get("settings").is_some_and(|s| !s.is_object() && !s.is_null()) {
        return Err(ApiError::BadRequest("`settings` must be an object".into()));
    }
    // Structural validation with the engine's own parser; the stored JSON stays the client's.
    serde_json::from_value::<hk_flow::Workflow>(Value::Object(m.clone()))
        .map_err(|e| ApiError::BadRequest(format!("invalid workflow: {e}")))?;
    for k in RESERVED {
        m.remove(k);
    }
    if m.get("settings").is_none_or(Value::is_null) {
        m.insert("settings".into(), json!({}));
    }
    m.entry("staticData").or_insert(Value::Null);
    m.entry("tags").or_insert_with(|| json!([]));
    Ok(Prepared { name, data: m })
}

/// n8n-shaped JSON for a stored workflow (id, name, active, nodes, ...).
pub fn workflow_json(r: &WorkflowRow) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(r.id));
    m.insert("name".into(), json!(r.name));
    m.insert("active".into(), json!(r.active));
    m.insert("createdAt".into(), json!(iso(r.created_ms)));
    m.insert("updatedAt".into(), json!(iso(r.updated_ms)));
    if let Ok(Value::Object(d)) = serde_json::from_str::<Value>(&r.data) {
        for (k, v) in d {
            m.entry(k).or_insert(v);
        }
    }
    m.insert("versionId".into(), json!(r.version_id));
    Value::Object(m)
}

#[derive(Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub(crate) struct WorkflowListQuery {
    /// Only active (`true`) or inactive (`false`) workflows.
    active: Option<bool>,
    /// Case-insensitive name substring.
    name: Option<String>,
    /// Page size, 1-250 (default 100).
    limit: Option<u32>,
    /// `nextCursor` from the previous page.
    cursor: Option<String>,
}

fn page_size(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn bad_cursor() -> ApiError {
    ApiError::BadRequest("invalid cursor".into())
}

#[utoipa::path(
    get, path = "/api/v1/workflows", tag = "workflows", params(WorkflowListQuery),
    responses((status = 200, description = "Page of workflows", body = crate::openapi::WorkflowList)),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn list_workflows(
    p: Principal,
    State(s): State<AppState>,
    Query(q): Query<WorkflowListQuery>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::WorkflowRead)?;
    let limit = page_size(q.limit);
    let offset: u32 = match q.cursor.as_deref() {
        None => 0,
        Some(c) => c.parse().map_err(|_| bad_cursor())?,
    };
    let mut rows = flow(&s)?.store.list_workflows(q.active, q.name.as_deref(), limit + 1, offset).await?;
    let next = (rows.len() > limit as usize).then(|| (offset + limit).to_string());
    rows.truncate(limit as usize);
    Ok(Json(json!({ "data": rows.iter().map(workflow_json).collect::<Vec<_>>(), "nextCursor": next })))
}

#[utoipa::path(
    post, path = "/api/v1/workflows", tag = "workflows",
    request_body = crate::openapi::WorkflowBody,
    responses((status = 200, description = "Created workflow", body = crate::openapi::WorkflowDoc),
              (status = 400, description = "Invalid workflow")),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn create_workflow(
    p: Principal,
    State(s): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::WorkflowWrite)?;
    let f = flow(&s)?;
    let w = prepare_workflow(body)?;
    let row = f
        .store
        .insert_workflow(&new_id()?, &w.name, &new_version_id()?, &Value::Object(w.data).to_string())
        .await?;
    Ok(Json(workflow_json(&row)))
}

async fn find_workflow(f: &FlowApi, id: &str) -> Result<WorkflowRow, ApiError> {
    f.store.get_workflow(id).await?.ok_or_else(|| ApiError::NotFound(format!("workflow `{id}` not found")))
}

#[utoipa::path(
    get, path = "/api/v1/workflows/{id}", tag = "workflows",
    params(("id" = String, Path, description = "Workflow id")),
    responses((status = 200, body = crate::openapi::WorkflowDoc), (status = 404, description = "Unknown id")),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn get_workflow(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::WorkflowRead)?;
    Ok(Json(workflow_json(&find_workflow(flow(&s)?, &id).await?)))
}

#[utoipa::path(
    put, path = "/api/v1/workflows/{id}", tag = "workflows",
    params(("id" = String, Path, description = "Workflow id")),
    request_body = crate::openapi::WorkflowBody,
    responses((status = 200, body = crate::openapi::WorkflowDoc), (status = 400), (status = 404)),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn update_workflow(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::WorkflowWrite)?;
    let f = flow(&s)?;
    let old = find_workflow(f, &id).await?;
    let w = prepare_workflow(body)?;
    let row = f
        .store
        .update_workflow(&id, &w.name, &new_version_id()?, &Value::Object(w.data).to_string())
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("workflow `{id}` not found")))?;
    if old.active {
        // Triggers must follow the new graph; if they can't, n8n leaves it deactivated.
        if let Some(a) = &f.activator {
            let _ = a.deactivate(&id).await;
            if let Err(e) = a.activate(&workflow_json(&row)).await {
                f.store.set_workflow_active(&id, false).await?;
                f.webhooks.unregister_owner(&id);
                return Err(ApiError::BadRequest(format!("Workflow could not be activated: {e}")));
            }
        }
    }
    Ok(Json(workflow_json(&row)))
}

#[utoipa::path(
    delete, path = "/api/v1/workflows/{id}", tag = "workflows",
    params(("id" = String, Path, description = "Workflow id")),
    responses((status = 200, description = "The deleted workflow", body = crate::openapi::WorkflowDoc), (status = 404)),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn delete_workflow(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::WorkflowWrite)?;
    let f = flow(&s)?;
    let row = find_workflow(f, &id).await?;
    if row.active {
        if let Some(a) = &f.activator {
            let _ = a.deactivate(&id).await;
        }
    }
    f.webhooks.unregister_owner(&id);
    f.store.delete_workflow(&id).await?;
    Ok(Json(workflow_json(&row)))
}

#[utoipa::path(
    post, path = "/api/v1/workflows/{id}/activate", tag = "workflows",
    params(("id" = String, Path, description = "Workflow id")),
    responses((status = 200, body = crate::openapi::WorkflowDoc), (status = 400, description = "Could not activate"), (status = 404)),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn activate_workflow(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::WorkflowWrite)?;
    let f = flow(&s)?;
    let row = find_workflow(f, &id).await?;
    if let Some(a) = &f.activator {
        if row.active {
            let _ = a.deactivate(&id).await;
        }
        a.activate(&workflow_json(&row))
            .await
            .map_err(|e| ApiError::BadRequest(format!("Workflow could not be activated: {e}")))?;
    }
    let row = f.store.set_workflow_active(&id, true).await?.unwrap_or(row);
    Ok(Json(workflow_json(&row)))
}

#[utoipa::path(
    post, path = "/api/v1/workflows/{id}/deactivate", tag = "workflows",
    params(("id" = String, Path, description = "Workflow id")),
    responses((status = 200, body = crate::openapi::WorkflowDoc), (status = 404)),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn deactivate_workflow(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::WorkflowWrite)?;
    let f = flow(&s)?;
    let row = find_workflow(f, &id).await?;
    if let Some(a) = &f.activator {
        a.deactivate(&id)
            .await
            .map_err(|e| ApiError::BadRequest(format!("Workflow could not be deactivated: {e}")))?;
    }
    f.webhooks.unregister_owner(&id);
    let row = f.store.set_workflow_active(&id, false).await?.unwrap_or(row);
    Ok(Json(workflow_json(&row)))
}

// ----------------------------------------------------------------- executions

const STATUSES: [&str; 4] = ["error", "success", "waiting", "canceled"];

fn execution_json(r: &ExecutionRow, include_data: bool) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(r.id));
    m.insert("finished".into(), json!(r.finished));
    m.insert("mode".into(), json!(r.mode));
    m.insert("retryOf".into(), Value::Null);
    m.insert("retrySuccessId".into(), Value::Null);
    m.insert("startedAt".into(), json!(iso(r.started_ms)));
    m.insert("stoppedAt".into(), r.stopped_ms.map_or(Value::Null, |t| json!(iso(t))));
    m.insert("workflowId".into(), json!(r.workflow_id));
    m.insert("waitTill".into(), Value::Null);
    m.insert("status".into(), json!(r.status));
    if include_data {
        let data = r.data.as_deref().and_then(|d| serde_json::from_str::<Value>(d).ok()).unwrap_or(Value::Null);
        m.insert("data".into(), data);
    }
    Value::Object(m)
}

#[derive(Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub(crate) struct ExecutionListQuery {
    /// Include the run data (can be large).
    include_data: Option<bool>,
    /// `error`, `success`, `waiting` or `canceled`.
    status: Option<String>,
    workflow_id: Option<String>,
    /// Page size, 1-250 (default 100).
    limit: Option<u32>,
    cursor: Option<String>,
}

#[utoipa::path(
    get, path = "/api/v1/executions", tag = "executions", params(ExecutionListQuery),
    responses((status = 200, body = crate::openapi::ExecutionList)),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn list_executions(
    p: Principal,
    State(s): State<AppState>,
    Query(q): Query<ExecutionListQuery>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::ExecutionRead)?;
    if let Some(st) = &q.status {
        if !STATUSES.contains(&st.as_str()) {
            return Err(ApiError::BadRequest(format!("status must be one of {}", STATUSES.join(", "))));
        }
    }
    let limit = page_size(q.limit);
    let before: Option<i64> = match q.cursor.as_deref() {
        None => None,
        Some(c) => Some(c.parse().map_err(|_| bad_cursor())?),
    };
    let filter = ExecutionFilter { workflow_id: q.workflow_id, status: q.status };
    let mut rows = flow(&s)?.store.list_executions(&filter, before, limit + 1).await?;
    let next = (rows.len() > limit as usize).then(|| rows[limit as usize - 1].id.to_string());
    rows.truncate(limit as usize);
    let inc = q.include_data.unwrap_or(false);
    Ok(Json(json!({ "data": rows.iter().map(|r| execution_json(r, inc)).collect::<Vec<_>>(), "nextCursor": next })))
}

#[derive(Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub(crate) struct IncludeData {
    include_data: Option<bool>,
}

async fn find_execution(f: &FlowApi, id: i64) -> Result<ExecutionRow, ApiError> {
    f.store.get_execution(id).await?.ok_or_else(|| ApiError::NotFound(format!("execution {id} not found")))
}

#[utoipa::path(
    get, path = "/api/v1/executions/{id}", tag = "executions",
    params(("id" = i64, Path, description = "Execution id"), IncludeData),
    responses((status = 200, body = crate::openapi::ExecutionDoc), (status = 404)),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn get_execution(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<IncludeData>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::ExecutionRead)?;
    let r = find_execution(flow(&s)?, id).await?;
    Ok(Json(execution_json(&r, q.include_data.unwrap_or(false))))
}

#[utoipa::path(
    delete, path = "/api/v1/executions/{id}", tag = "executions",
    params(("id" = i64, Path, description = "Execution id")),
    responses((status = 200, description = "The deleted execution", body = crate::openapi::ExecutionDoc), (status = 404)),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn delete_execution(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    // Destroying run history is a workflow-management action.
    p.require(Scope::WorkflowWrite)?;
    let f = flow(&s)?;
    let r = find_execution(f, id).await?;
    f.store.delete_execution(id).await?;
    Ok(Json(execution_json(&r, false)))
}

// ---------------------------------------------------------------- credentials

fn credential_json(r: &CredentialRow) -> Value {
    json!({ "id": r.id, "name": r.name, "type": r.kind, "createdAt": iso(r.created_ms), "updatedAt": iso(r.updated_ms) })
}

fn credential_secret_name(id: &str) -> String {
    format!("credential.{id}")
}

/// `data` must satisfy the type's schema: no unknown keys, required keys present, strings
/// where the schema says string. Error messages name fields, never values.
fn validate_credential(schema: &Value, data: &Map<String, Value>) -> Result<(), ApiError> {
    let props = schema.get("properties").and_then(Value::as_object);
    for (k, v) in data {
        let Some(def) = props.and_then(|p| p.get(k)) else {
            return Err(ApiError::BadRequest(format!("`data` has unknown property `{k}`")));
        };
        if def.get("type").and_then(Value::as_str) == Some("string") && !v.is_string() {
            return Err(ApiError::BadRequest(format!("`data.{k}` must be a string")));
        }
    }
    for r in schema.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        if !data.contains_key(r) {
            return Err(ApiError::BadRequest(format!("`data` is missing required property `{r}`")));
        }
    }
    Ok(())
}

#[utoipa::path(
    post, path = "/api/v1/credentials", tag = "credentials",
    request_body = crate::openapi::CredentialBody,
    responses((status = 200, description = "Credential metadata; the data is never returned", body = crate::openapi::CredentialDoc),
              (status = 400)),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn create_credential(
    p: Principal,
    State(s): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::CredentialWrite)?;
    let f = flow(&s)?;
    let name = body.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty() && n.chars().count() <= MAX_NAME);
    let kind = body.get("type").and_then(Value::as_str).filter(|t| !t.is_empty());
    let (Some(name), Some(kind)) = (name, kind) else {
        return Err(ApiError::BadRequest("`name` and `type` must be non-empty strings".into()));
    };
    let Some(Value::Object(data)) = body.get("data") else {
        return Err(ApiError::BadRequest("`data` must be an object".into()));
    };
    let schema = f
        .credential_schemas
        .schema(kind)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown credential type `{kind}`")))?;
    validate_credential(&schema, data)?;

    let id = new_id()?;
    let sname = credential_secret_name(&id);
    f.store.put_secret(f.secrets.as_ref(), &sname, Secret::new(Value::Object(data.clone()).to_string())).await?;
    match f.store.insert_credential(&id, name, kind).await {
        Ok(row) => Ok(Json(credential_json(&row))),
        Err(e) => {
            let _ = f.store.delete_secret(f.secrets.as_ref(), &sname).await;
            Err(e.into())
        }
    }
}

#[utoipa::path(
    delete, path = "/api/v1/credentials/{id}", tag = "credentials",
    params(("id" = String, Path, description = "Credential id")),
    responses((status = 200, description = "Metadata of the deleted credential", body = crate::openapi::CredentialDoc), (status = 404)),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn delete_credential(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::CredentialWrite)?;
    let f = flow(&s)?;
    let row = f
        .store
        .get_credential(&id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("credential `{id}` not found")))?;
    f.store.delete_secret(f.secrets.as_ref(), &credential_secret_name(&id)).await?;
    f.store.delete_credential(&id).await?;
    Ok(Json(credential_json(&row)))
}

#[utoipa::path(
    get, path = "/api/v1/credentials/schema/{credentialTypeName}", tag = "credentials",
    params(("credentialTypeName" = String, Path, description = "Credential type, e.g. httpBasicAuth")),
    responses((status = 200, description = "JSON schema of the credential data"), (status = 404)),
    security(("bearer" = []), ("n8nApiKey" = []))
)]
pub(crate) async fn credential_schema(
    p: Principal,
    State(s): State<AppState>,
    Path(type_name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::CredentialWrite)?;
    flow(&s)?
        .credential_schemas
        .schema(&type_name)
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("credential type `{type_name}` not found")))
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn iso_known_instants() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_709_210_096_789), "2024-02-29T12:34:56.789Z");
        assert_eq!(iso(1_000_000_000_000), "2001-09-09T01:46:40.000Z");
    }

    #[test]
    fn workflow_prep_rejects_bad_shapes_and_strips_reserved() {
        assert!(prepare_workflow(json!([])).is_err());
        assert!(prepare_workflow(json!({"name":"","nodes":[],"connections":{}})).is_err());
        assert!(prepare_workflow(json!({"name":"a","nodes":{},"connections":{}})).is_err());
        assert!(prepare_workflow(json!({"name":"a","nodes":[],"connections":[]})).is_err());
        let w = prepare_workflow(json!({"name":"a","id":"x","active":true,"nodes":[],"connections":{}})).unwrap();
        assert!(!w.data.contains_key("id") && !w.data.contains_key("active"));
        assert_eq!(w.data["settings"], json!({}));
    }

    #[test]
    fn credential_validation_names_fields_not_values() {
        let schema = crate::flow::CredentialSchemas::schema(&crate::flow::BuiltinCredentialSchemas, "httpBasicAuth").unwrap();
        let ok: Map<String, Value> = serde_json::from_value(json!({"user":"u","password":"p"})).unwrap();
        assert!(validate_credential(&schema, &ok).is_ok());
        let extra: Map<String, Value> = serde_json::from_value(json!({"user":"u","password":"p","x":"hunter2"})).unwrap();
        let e = validate_credential(&schema, &extra).unwrap_err().to_string();
        assert!(e.contains("`x`") && !e.contains("hunter2"));
        let missing: Map<String, Value> = serde_json::from_value(json!({"user":"u"})).unwrap();
        assert!(validate_credential(&schema, &missing).is_err());
        let wrong: Map<String, Value> = serde_json::from_value(json!({"user":1,"password":"p"})).unwrap();
        assert!(validate_credential(&schema, &wrong).is_err());
    }
}
