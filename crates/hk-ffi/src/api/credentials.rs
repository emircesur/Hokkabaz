//! Flow credentials. Secret values go in via `credential_create`/`credential_update` and never
//! come back: lists carry metadata, the edit form asks only which fields are set.

use serde_json::{Map, Value, json};

use crate::credentials::global;
use crate::error::HkError;

fn to_json(v: &Value) -> Result<String, HkError> {
    serde_json::to_string(v).map_err(|e| HkError::internal(e.to_string()))
}

fn fields(data_json: &str) -> Result<Map<String, Value>, HkError> {
    match serde_json::from_str(data_json) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(HkError::invalid("credential data must be a JSON object")),
        Err(e) => Err(HkError::invalid(format!("credential data: {e}"))),
    }
}

/// Credential types the registry knows: `[{name, display_name, documentation_url, fields: [..]}]`
/// where each field is `{name, display_name, type, required, secret, description, default, options}`.
pub async fn credential_types() -> Result<String, HkError> {
    let env = crate::nodes::env();
    let mut names = env.cred_types.names();
    names.sort();
    let list: Vec<Value> = names
        .iter()
        .filter_map(|n| env.cred_types.resolved(n))
        .map(|t| {
            let fields: Vec<Value> = t
                .properties
                .iter()
                .map(|p| {
                    let v = serde_json::to_value(p).unwrap_or(Value::Null);
                    json!({
                        "name": p.name,
                        "display_name": p.display_name,
                        "type": v.get("type").cloned().unwrap_or(Value::Null),
                        "required": p.required.unwrap_or(false),
                        "secret": v.pointer("/typeOptions/password").and_then(Value::as_bool).unwrap_or(false),
                        "description": p.description,
                        "default": p.default,
                        "options": v.get("options").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            json!({"name": t.name, "display_name": t.display_name, "documentation_url": t.documentation_url, "fields": fields})
        })
        .collect();
    to_json(&Value::Array(list))
}

/// `[{id, name, type, updated_ms}]`, optionally only one type.
pub async fn credential_list(kind: Option<String>) -> Result<String, HkError> {
    let rows = global()?.list(kind.as_deref()).await?;
    to_json(&serde_json::to_value(rows).map_err(|e| HkError::internal(e.to_string()))?)
}

/// `data_json` is an object of field name -> value. Returns the new credential as JSON.
pub async fn credential_create(
    name: String,
    kind: String,
    data_json: String,
) -> Result<String, HkError> {
    let info = global()?.create(&name, &kind, fields(&data_json)?).await?;
    to_json(&serde_json::to_value(info).map_err(|e| HkError::internal(e.to_string()))?)
}

/// Rename and/or replace the values (`data_json` replaces the whole object).
pub async fn credential_update(
    id: String,
    name: Option<String>,
    data_json: Option<String>,
) -> Result<(), HkError> {
    let data = data_json.as_deref().map(fields).transpose()?;
    global()?.update(&id, name.as_deref(), data).await
}

pub async fn credential_delete(id: String) -> Result<(), HkError> {
    global()?.delete(&id).await
}

/// Names of the fields that have a value (for the edit form; values are never returned).
pub async fn credential_set_fields(id: String) -> Result<Vec<String>, HkError> {
    global()?.set_fields(&id).await
}

/// Start the browser sign-in of an OAuth credential type from the (partly filled) form. Returns
/// `{state, auth_url, redirect_uri}`: open `auth_url` in the browser, make sure `redirect_uri` is
/// registered in the provider's app settings, then call `oauth_finish(state)`.
pub async fn oauth_begin(kind: String, data_json: String) -> Result<String, HkError> {
    let data = fields(&data_json)?;
    let env = crate::nodes::env();
    let ty = env
        .cred_types
        .resolved(&kind)
        .ok_or_else(|| HkError::invalid(format!("unknown credential type `{kind}`")))?;
    let merged = ty.with_defaults(&data);
    let begin =
        hk_integrations::oauth_flow::begin(hk_integrations::oauth_flow::Params { data: merged })
            .await
            .map_err(HkError::invalid)?;
    to_json(&serde_json::to_value(begin).map_err(|e| HkError::internal(e.to_string()))?)
}

/// Wait until the sign-in started by `oauth_begin` completes (the user approved in the browser).
/// Returns the credential fields it fills: `{refreshToken?, accessToken?, instanceUrl?}`.
pub async fn oauth_finish(state: String) -> Result<String, HkError> {
    let tokens = hk_integrations::oauth_flow::finish(&state)
        .await
        .map_err(|m| HkError::new(crate::error::HkErrorKind::Provider, m))?;
    to_json(&serde_json::to_value(tokens).map_err(|e| HkError::internal(e.to_string()))?)
}

/// Abandon a sign-in (the user closed the dialog).
#[flutter_rust_bridge::frb(sync)]
pub fn oauth_cancel(state: String) {
    hk_integrations::oauth_flow::cancel(&state);
}
