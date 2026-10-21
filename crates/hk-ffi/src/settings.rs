//! Persisted settings (`hk-store` settings table, key `core.settings`) and their translation to
//! `hk_llm` provider configs. API keys are never part of this: they live in the secret store and
//! are referenced by name.

use std::path::{Path, PathBuf};

use hk_llm::catalog::Catalog;
use hk_llm::registry::{KeyRef, ProviderConfig, ProviderKind, ProviderOptions, RegistryError};
use hk_llm::{Capabilities, ModelInfo, Route};
use serde::{Deserialize, Serialize};

use crate::dto::{ModeDto, ModelDto, ProviderInputDto, ProviderKindDto};
use crate::error::{HkError, HkErrorKind};

pub const SETTINGS_KEY: &str = "core.settings";
const BUILTIN: [(&str, ProviderKindDto); 4] = [
    ("deepseek", ProviderKindDto::Deepseek),
    ("anthropic", ProviderKindDto::Anthropic),
    ("gemini", ProviderKindDto::Gemini),
    ("ollama", ProviderKindDto::Ollama),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredProvider {
    pub id: String,
    /// `ProviderKind` string (`deepseek`, `openai_compat`, ...).
    pub kind: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stored {
    #[serde(default)]
    pub providers: Vec<StoredProvider>,
    #[serde(default)]
    pub default_model: Option<String>,
    /// `build` | `plan`.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub workspace_root: Option<String>,
}

impl Stored {
    /// Parse stored JSON (missing = fresh install) and make sure every built-in is present.
    pub fn load(raw: Option<&str>) -> Result<Self, HkError> {
        let mut s: Self = match raw {
            Some(r) => serde_json::from_str(r)?,
            None => Self::default(),
        };
        for (id, kind) in BUILTIN.iter().rev() {
            if !s.providers.iter().any(|p| p.id == *id) {
                s.providers.insert(
                    0,
                    StoredProvider {
                        id: (*id).to_owned(),
                        kind: kind_str(*kind).to_owned(),
                        base_url: None,
                        // A fresh install starts with the one provider `hk chat` also defaults to.
                        enabled: *id == "deepseek",
                        models: Vec::new(),
                    },
                );
            }
        }
        Ok(s)
    }

    pub fn mode(&self) -> ModeDto {
        match self.mode.as_deref() {
            Some("plan") => ModeDto::Plan,
            _ => ModeDto::Build,
        }
    }

    pub fn set_mode(&mut self, m: ModeDto) {
        self.mode = Some(
            match m {
                ModeDto::Build => "build",
                ModeDto::Plan => "plan",
            }
            .to_owned(),
        );
    }

    /// Insert or update from user input, after validation.
    pub fn upsert(&mut self, input: ProviderInputDto) -> Result<(), HkError> {
        validate_id(&input.id)?;
        let builtin = is_builtin(&input.id);
        if builtin && BUILTIN.iter().any(|(id, k)| *id == input.id && *k != input.kind) {
            return Err(HkError::invalid(format!("provider `{}` is a built-in of another kind", input.id)));
        }
        if !builtin && !matches!(input.kind, ProviderKindDto::OpenaiCompat | ProviderKindDto::Ollama) {
            return Err(HkError::invalid(
                "custom providers must be `openai_compat` or `ollama` (other kinds have fixed ids)",
            ));
        }
        let base_url = input.base_url.map(|u| u.trim().trim_end_matches('/').to_owned()).filter(|u| !u.is_empty());
        if let Some(u) = &base_url {
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                return Err(HkError::invalid("base_url must start with http:// or https://"));
            }
        }
        if input.kind == ProviderKindDto::OpenaiCompat && base_url.is_none() {
            return Err(HkError::invalid("openai_compat providers need a base_url"));
        }
        let models: Vec<String> = input.models.into_iter().map(|m| m.trim().to_owned()).filter(|m| !m.is_empty()).collect();
        let new = StoredProvider {
            id: input.id,
            kind: kind_str(input.kind).to_owned(),
            base_url,
            enabled: input.enabled,
            models,
        };
        match self.providers.iter_mut().find(|p| p.id == new.id) {
            Some(slot) => *slot = new,
            None => self.providers.push(new),
        }
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<(), HkError> {
        if is_builtin(id) {
            return Err(HkError::new(
                HkErrorKind::Conflict,
                format!("`{id}` is built in; disable it instead"),
            ));
        }
        let before = self.providers.len();
        self.providers.retain(|p| p.id != id);
        if self.providers.len() == before {
            return Err(HkError::new(HkErrorKind::NotFound, format!("no provider `{id}`")));
        }
        Ok(())
    }

    pub fn find(&self, id: &str) -> Option<&StoredProvider> {
        self.providers.iter().find(|p| p.id == id)
    }
}

pub fn is_builtin(id: &str) -> bool {
    BUILTIN.iter().any(|(b, _)| *b == id)
}

fn validate_id(id: &str) -> Result<(), HkError> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(HkError::invalid("provider id must be 1-64 chars of [a-z0-9_-]"))
    }
}

/// Secret-store name holding a provider's API key.
pub fn key_name(provider_id: &str) -> String {
    format!("provider/{provider_id}/api_key")
}

/// `provider/model` ref check.
pub fn validate_model_ref(m: &str) -> Result<(), HkError> {
    Route::parse(m).map(drop).map_err(|_| HkError::invalid(format!("model must be `provider/model`, got `{m}`")))
}

/// Absolute path, created if missing.
pub fn prepare_workspace(p: &str) -> Result<PathBuf, HkError> {
    let path = Path::new(p.trim());
    if p.trim().is_empty() || !path.is_absolute() {
        return Err(HkError::invalid("workspace_root must be an absolute path"));
    }
    std::fs::create_dir_all(path).map_err(|e| HkError::invalid(format!("workspace_root: {e}")))?;
    Ok(path.to_owned())
}

/// Provider configs for every enabled provider. `has_key` tells whether the secret exists:
/// without a key ref, kinds that need one fail with `MissingKey` (surfaced per provider).
pub fn to_configs(s: &Stored, has_key: impl Fn(&str) -> bool) -> Vec<ProviderConfig> {
    s.providers
        .iter()
        .filter(|p| p.enabled)
        .map(|p| ProviderConfig {
            id: p.id.clone(),
            kind: p.kind.clone(),
            base_url: p.base_url.clone(),
            api_key: has_key(&p.id).then(|| KeyRef::new(key_name(&p.id))),
            options: ProviderOptions {
                models: p.models.iter().map(|m| fixed_model(&p.id, m)).collect(),
                ..ProviderOptions::default()
            },
        })
        .collect()
}

fn fixed_model(provider: &str, id: &str) -> ModelInfo {
    ModelInfo {
        id: id.to_owned(),
        provider: provider.to_owned(),
        name: id.to_owned(),
        context_window: 0,
        max_output: None,
        // User-listed models are assumed agent-capable; the catalog corrects this when it knows.
        capabilities: Capabilities { tools: true, reasoning: false, vision: false },
        cost: None,
    }
}

pub fn registry_error_provider(e: &RegistryError) -> &str {
    match e {
        RegistryError::MissingKey { provider }
        | RegistryError::UnknownKind { provider, .. }
        | RegistryError::MissingBaseUrl { provider }
        | RegistryError::Duplicate { provider }
        | RegistryError::Build { provider, .. } => provider,
    }
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
    match ProviderKind::parse(s) {
        Some(ProviderKind::Deepseek) => ProviderKindDto::Deepseek,
        Some(ProviderKind::Anthropic) => ProviderKindDto::Anthropic,
        Some(ProviderKind::Gemini) => ProviderKindDto::Gemini,
        Some(ProviderKind::Ollama) => ProviderKindDto::Ollama,
        Some(ProviderKind::OpenaiCompat) | None => ProviderKindDto::OpenaiCompat,
    }
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

/// Catalog helper kept here so `local.rs` stays about wiring.
pub fn load_cached_catalog(path: &Path) -> Option<Catalog> {
    hk_llm::catalog::CatalogStore::new(path).load_cached()
}
