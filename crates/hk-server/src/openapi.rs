//! OpenAPI 3 document for the n8n-shaped surface, served at `/api/v1/openapi.json`.
//!
//! Covers webhooks and `/api/v1` workflows / executions / credentials. The older agent routes
//! (sessions, turns, ws) are not annotated yet.

// The schema structs below exist only to describe wire shapes; nothing reads their fields.
#![allow(dead_code)]

use axum::Json;
use serde_json::Value;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};

#[derive(ToSchema)]
#[schema(as = WorkflowBody)]
pub(crate) struct WorkflowBody {
    name: String,
    #[schema(value_type = Vec<Object>)]
    nodes: Vec<Value>,
    #[schema(value_type = Object)]
    connections: Value,
    #[schema(value_type = Option<Object>)]
    settings: Option<Value>,
    #[schema(value_type = Option<Object>)]
    static_data: Option<Value>,
}

/// n8n workflow JSON plus server-owned fields.
#[derive(ToSchema)]
#[schema(as = Workflow)]
pub(crate) struct WorkflowDoc {
    id: String,
    name: String,
    active: bool,
    created_at: String,
    updated_at: String,
    version_id: String,
    #[schema(value_type = Vec<Object>)]
    nodes: Vec<Value>,
    #[schema(value_type = Object)]
    connections: Value,
    #[schema(value_type = Object)]
    settings: Value,
}

#[derive(ToSchema)]
pub(crate) struct WorkflowList {
    data: Vec<WorkflowDoc>,
    next_cursor: Option<String>,
}

#[derive(ToSchema)]
#[schema(as = Execution)]
pub(crate) struct ExecutionDoc {
    id: i64,
    finished: bool,
    mode: String,
    started_at: String,
    stopped_at: Option<String>,
    workflow_id: String,
    /// `error`, `success`, `waiting`, `canceled`, `running`.
    status: String,
    /// n8n `{resultData: {runData}}`; only with `includeData=true`.
    #[schema(value_type = Option<Object>)]
    data: Option<Value>,
}

#[derive(ToSchema)]
pub(crate) struct ExecutionList {
    data: Vec<ExecutionDoc>,
    next_cursor: Option<String>,
}

#[derive(ToSchema)]
pub(crate) struct CredentialBody {
    name: String,
    /// Credential type, e.g. `httpBasicAuth`.
    r#type: String,
    /// Write-only; validated against the type's schema, stored in the secret store.
    #[schema(value_type = Object)]
    data: Value,
}

#[derive(ToSchema)]
#[schema(as = Credential)]
pub(crate) struct CredentialDoc {
    id: String,
    name: String,
    r#type: String,
    created_at: String,
    updated_at: String,
}

// Webhook paths: the runtime routes are catch-all, so they are described by stubs.

/// Production webhook (active workflows). Any method the webhook is configured for.
#[utoipa::path(get, path = "/webhook/{path}", tag = "webhooks",
    params(("path" = String, Path, description = "Webhook path")),
    responses((status = 200, description = "Answer per the webhook's response mode"),
              (status = 401, description = "Bad HMAC signature or token"),
              (status = 404, description = "Not registered"),
              (status = 413, description = "Body over the webhook's limit")))]
fn webhook_get() {}

/// Production webhook; JSON, form, multipart (text parts) or raw body.
#[utoipa::path(post, path = "/webhook/{path}", tag = "webhooks",
    params(("path" = String, Path, description = "Webhook path")),
    request_body(content = String, description = "JSON, urlencoded, multipart or raw", content_type = "*/*"),
    responses((status = 200, description = "Answer per the webhook's response mode"),
              (status = 401, description = "Bad HMAC signature or token"),
              (status = 404, description = "Not registered"),
              (status = 413, description = "Body over the webhook's limit")))]
fn webhook_post() {}

/// Test webhook: answers once after the editor arms it.
#[utoipa::path(get, path = "/webhook-test/{path}", tag = "webhooks",
    params(("path" = String, Path, description = "Webhook path")),
    responses((status = 200, description = "Answer per the webhook's response mode"),
              (status = 404, description = "Test listener not armed")))]
fn webhook_test_get() {}

/// Test webhook: answers once after the editor arms it.
#[utoipa::path(post, path = "/webhook-test/{path}", tag = "webhooks",
    params(("path" = String, Path, description = "Webhook path")),
    request_body(content = String, content_type = "*/*"),
    responses((status = 200, description = "Answer per the webhook's response mode"),
              (status = 404, description = "Test listener not armed")))]
fn webhook_test_post() {}

struct Security;

impl Modify for Security {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let c = openapi.components.get_or_insert_with(Default::default);
        c.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .description(Some("Instance token"))
                    .build(),
            ),
        );
        c.add_security_scheme(
            "n8nApiKey",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-N8N-API-KEY",
                "Same token, n8n header name",
            ))),
        );
    }
}

#[derive(OpenApi, Debug)]
#[openapi(
    info(title = "Hokkabaz n8n-compatible API", description = "Webhooks and the n8n public-API subset."),
    paths(
        webhook_get, webhook_post, webhook_test_get, webhook_test_post,
        crate::n8n::list_workflows, crate::n8n::create_workflow, crate::n8n::get_workflow,
        crate::n8n::update_workflow, crate::n8n::delete_workflow, crate::n8n::activate_workflow,
        crate::n8n::deactivate_workflow, crate::n8n::run_workflow,
        crate::n8n::list_executions, crate::n8n::get_execution, crate::n8n::delete_execution,
        crate::n8n::create_credential, crate::n8n::delete_credential, crate::n8n::credential_schema,
    ),
    components(schemas(
        WorkflowBody, WorkflowDoc, WorkflowList, crate::n8n::RunBody, ExecutionDoc, ExecutionList, CredentialBody, CredentialDoc
    )),
    modifiers(&Security)
)]
pub struct ApiDoc;

pub(crate) async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}
