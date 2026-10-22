//! Settings + models. API keys go in via `set_api_key` and never come back: `ProviderDto`
//! carries only `has_key`.

use crate::dto::{ModelListDto, PreferencesDto, ProviderInputDto, SettingsDto};
use crate::error::HkError;
use crate::handle::core;

pub async fn get_settings() -> Result<SettingsDto, HkError> {
    core()?.get_settings().await
}

/// Insert or update a provider (built-in ids keep their kind; custom ones are `OpenaiCompat`
/// or `Ollama`). Returns the refreshed settings, including per-provider `error`.
pub async fn save_provider(input: ProviderInputDto) -> Result<SettingsDto, HkError> {
    core()?.save_provider(input).await
}

/// Remove a custom provider and its key.
pub async fn remove_provider(provider_id: String) -> Result<SettingsDto, HkError> {
    core()?.remove_provider(provider_id).await
}

/// Store (or with `None`/empty, delete) a provider's API key.
pub async fn set_api_key(provider_id: String, key: Option<String>) -> Result<SettingsDto, HkError> {
    core()?.set_api_key(provider_id, key).await
}

/// Default model (`provider/model`), mode and workspace root.
pub async fn set_preferences(prefs: PreferencesDto) -> Result<SettingsDto, HkError> {
    core()?.set_preferences(prefs).await
}

/// Models of every enabled provider, enriched from the models.dev catalog. A provider that is
/// offline or misconfigured shows up in `errors`; the rest are still listed.
pub async fn list_models() -> Result<ModelListDto, HkError> {
    core()?.list_models().await
}

/// Re-fetch the models.dev catalog (best effort; the cached copy stays on failure).
pub async fn refresh_catalog() -> Result<(), HkError> {
    core()?.refresh_catalog().await
}

/// Known OpenAI-compatible services (hosted and local) as a JSON array of
/// `{id, label, kind, base_url, env, local}`; the "add endpoint" dialog prefills from these.
#[flutter_rust_bridge::frb(sync)]
pub fn provider_presets() -> String {
    serde_json::to_string(hk_providers::PRESETS).unwrap_or_else(|_| "[]".to_owned())
}
