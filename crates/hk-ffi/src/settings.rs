//! DTO glue over `hk-providers` (the settings model, shared with `hokkabazd`).

use std::path::{Path, PathBuf};

use hk_llm::ModelInfo;
use hk_providers::{ProviderInput, SettingsView};

use crate::dto::{ModeDto, ModelDto, ProviderDto, ProviderInputDto, ProviderKindDto};
use crate::error::HkError;

pub use hk_providers::settings::validate_model_ref;

/// Absolute path, created if missing.
pub fn prepare_workspace(p: &str) -> Result<PathBuf, HkError> {
    let path = Path::new(p.trim());
    if p.trim().is_empty() || !path.is_absolute() {
        return Err(HkError::invalid("workspace_root must be an absolute path"));
    }
    std::fs::create_dir_all(path).map_err(|e| HkError::invalid(format!("workspace_root: {e}")))?;
    Ok(path.to_owned())
}

pub fn kind_str(k: ProviderKindDto) -> &'static str {
    match k {
        ProviderKindDto::Deepseek => "deepseek",
        ProviderKindDto::OpenaiCompat => "openai_compat",
        ProviderKindDto::Anthropic => "anthropic",
        ProviderKindDto::Gemini => "gemini",
        ProviderKindDto::Ollama => "ollama",
    }
}

pub fn kind_dto(s: &str) -> ProviderKindDto {
    match s {
        "deepseek" => ProviderKindDto::Deepseek,
        "anthropic" => ProviderKindDto::Anthropic,
        "gemini" => ProviderKindDto::Gemini,
        "ollama" => ProviderKindDto::Ollama,
        _ => ProviderKindDto::OpenaiCompat,
    }
}

pub fn input(i: ProviderInputDto) -> ProviderInput {
    ProviderInput {
        id: i.id,
        kind: kind_str(i.kind).to_owned(),
        base_url: i.base_url,
        enabled: i.enabled,
        models: i.models,
    }
}

pub fn mode(plan: bool) -> ModeDto {
    if plan { ModeDto::Plan } else { ModeDto::Build }
}

pub fn provider_dtos(v: &SettingsView) -> Vec<ProviderDto> {
    v.providers
        .iter()
        .map(|p| ProviderDto {
            id: p.id.clone(),
            kind: kind_dto(&p.kind),
            base_url: p.base_url.clone(),
            enabled: p.enabled,
            has_key: p.has_key,
            builtin: p.builtin,
            models: p.models.clone(),
            error: p.error.clone(),
        })
        .collect()
}

pub fn model_dto(m: &ModelInfo) -> ModelDto {
    ModelDto {
        reference: format!("{}/{}", m.provider, m.id),
        id: m.id.clone(),
        provider: m.provider.clone(),
        name: m.name.clone(),
        context_window: i64::try_from(m.context_window).unwrap_or(i64::MAX),
        max_output: m.max_output.map(|v| i64::try_from(v).unwrap_or(i64::MAX)),
        tools: m.capabilities.tools,
        reasoning: m.capabilities.reasoning,
        vision: m.capabilities.vision,
        input_per_mtok: m.cost.map(|c| c.input),
        output_per_mtok: m.cost.map(|c| c.output),
    }
}
