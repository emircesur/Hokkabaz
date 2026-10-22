//! `/api/v1/settings` + `/api/v1/providers`: the instance's model providers, so a connected app
//! can set up Anthropic / OpenAI / Ollama / ... on a headless daemon exactly like it does locally.
//! Reads need `read`; every change needs `admin`. Keys are write-only.

use axum::Json;
use axum::extract::{Path, State};
use hk_providers::{
    PRESETS, Preferences, ProviderHub, ProviderInput, ProvidersError, SettingsView,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::AppState;
use crate::auth::{Principal, Scope};
use crate::error::ApiError;

impl From<ProvidersError> for ApiError {
    fn from(e: ProvidersError) -> Self {
        match e {
            ProvidersError::Invalid(m) => Self::BadRequest(m),
            ProvidersError::NotFound(m) => Self::NotFound(m),
            ProvidersError::Conflict(m) => Self::Conflict(m),
            // Provider messages name the endpoint / missing key, never key material.
            ProvidersError::Provider(m) => Self::BadRequest(m),
            e @ (ProvidersError::Store(_) | ProvidersError::Json(_)) => {
                Self::Internal(e.to_string())
            }
        }
    }
}

fn hub(s: &AppState) -> Result<&ProviderHub, ApiError> {
    s.providers
        .as_deref()
        .ok_or_else(|| ApiError::NotFound("providers are not enabled on this instance".into()))
}

fn body(v: &SettingsView) -> Json<Value> {
    Json(json!({ "settings": v, "presets": PRESETS }))
}

pub(crate) async fn get_settings(
    p: Principal,
    State(s): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Read)?;
    Ok(body(&hub(&s)?.view().await))
}

pub(crate) async fn save_provider(
    p: Principal,
    State(s): State<AppState>,
    Json(input): Json<ProviderInput>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Admin)?;
    Ok(body(&hub(&s)?.save_provider(input).await?))
}

pub(crate) async fn remove_provider(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Admin)?;
    Ok(body(&hub(&s)?.remove_provider(&id).await?))
}

#[derive(Deserialize)]
pub(crate) struct KeyBody {
    key: Option<String>,
}

pub(crate) async fn set_key(
    p: Principal,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<KeyBody>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Admin)?;
    Ok(body(&hub(&s)?.set_api_key(&id, b.key.as_deref()).await?))
}

#[derive(Deserialize)]
pub(crate) struct PrefsBody {
    default_model: Option<String>,
    #[serde(default)]
    plan_mode: bool,
}

pub(crate) async fn set_preferences(
    p: Principal,
    State(s): State<AppState>,
    Json(b): Json<PrefsBody>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Admin)?;
    let hub = hub(&s)?;
    // The daemon's workspace is its own business (`--workspace`); remote apps don't move it.
    let workspace_root = hub.stored().await.workspace_root;
    Ok(body(
        &hub.set_preferences(Preferences {
            default_model: b.default_model,
            plan_mode: b.plan_mode,
            workspace_root,
        })
        .await?,
    ))
}

pub(crate) async fn models(
    p: Principal,
    State(s): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Read)?;
    Ok(Json(json!(hub(&s)?.list_models().await)))
}

pub(crate) async fn refresh_catalog(
    p: Principal,
    State(s): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    p.require(Scope::Admin)?;
    hub(&s)?.refresh_catalog().await?;
    Ok(Json(json!({ "ok": true })))
}
