//! Persisted provider settings (`hk-store` settings table, key `core.settings`) and their
//! translation to `hk_llm` provider configs. API keys are never part of this: they live in the
//! secret store and are referenced by name.
//!
//! Same JSON the app core has always stored, so a data dir moves between app and daemon.

use hk_llm::registry::{KeyRef, ProviderConfig, ProviderKind, ProviderOptions, RegistryError};
use hk_llm::{Capabilities, ModelInfo, Route};
use serde::{Deserialize, Serialize};

use crate::error::ProvidersError;

pub const SETTINGS_KEY: &str = "core.settings";

/// Fixed-id providers with a dedicated adapter. They cannot be removed, only disabled.
pub const BUILTIN: [(&str, &str); 4] = [
    ("deepseek", "deepseek"),
    ("anthropic", "anthropic"),
    ("gemini", "gemini"),
    ("ollama", "ollama"),
];

/// A hosted or local OpenAI-compatible service we know the endpoint of. A preset whose env var is
/// set is added (enabled) on start when the key backend is the environment (daemon), and the UI
/// offers the rest as one-tap additions.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: &'static str,
    pub base_url: &'static str,
    /// Env var that carries the key (`""` = none needed).
    pub env: &'static str,
    /// Runs on this machine / LAN: no key, no cloud.
    pub local: bool,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "openai",
        label: "OpenAI",
        kind: "openai_compat",
        base_url: "https://api.openai.com/v1",
        env: "OPENAI_API_KEY",
        local: false,
    },
    Preset {
        id: "openrouter",
        label: "OpenRouter",
        kind: "openai_compat",
        base_url: "https://openrouter.ai/api/v1",
        env: "OPENROUTER_API_KEY",
        local: false,
    },
    Preset {
        id: "groq",
        label: "Groq",
        kind: "openai_compat",
        base_url: "https://api.groq.com/openai/v1",
        env: "GROQ_API_KEY",
        local: false,
    },
    Preset {
        id: "mistral",
        label: "Mistral",
        kind: "openai_compat",
        base_url: "https://api.mistral.ai/v1",
        env: "MISTRAL_API_KEY",
        local: false,
    },
    Preset {
        id: "xai",
        label: "xAI",
        kind: "openai_compat",
        base_url: "https://api.x.ai/v1",
        env: "XAI_API_KEY",
        local: false,
    },
    Preset {
        id: "together",
        label: "Together AI",
        kind: "openai_compat",
        base_url: "https://api.together.xyz/v1",
        env: "TOGETHER_API_KEY",
        local: false,
    },
    Preset {
        id: "fireworks",
        label: "Fireworks",
        kind: "openai_compat",
        base_url: "https://api.fireworks.ai/inference/v1",
        env: "FIREWORKS_API_KEY",
        local: false,
    },
    Preset {
        id: "cerebras",
        label: "Cerebras",
        kind: "openai_compat",
        base_url: "https://api.cerebras.ai/v1",
        env: "CEREBRAS_API_KEY",
        local: false,
    },
    Preset {
        id: "moonshot",
        label: "Moonshot (Kimi)",
        kind: "openai_compat",
        base_url: "https://api.moonshot.ai/v1",
        env: "MOONSHOT_API_KEY",
        local: false,
    },
    Preset {
        id: "zhipu",
        label: "Z.ai (GLM)",
        kind: "openai_compat",
        base_url: "https://api.z.ai/api/paas/v4",
        env: "ZAI_API_KEY",
        local: false,
    },
    Preset {
        id: "lmstudio",
        label: "LM Studio",
        kind: "openai_compat",
        base_url: "http://localhost:1234/v1",
        env: "",
        local: true,
    },
    Preset {
        id: "llamacpp",
        label: "llama.cpp server",
        kind: "openai_compat",
        base_url: "http://localhost:8080/v1",
        env: "",
        local: true,
    },
    Preset {
        id: "vllm",
        label: "vLLM",
        kind: "openai_compat",
        base_url: "http://localhost:8000/v1",
        env: "",
        local: true,
    },
];

/// Env var holding the key of provider `id` (`None` = no convention for this id).
pub fn env_key_var(id: &str) -> Option<String> {
    let known = match id {
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        _ => PRESETS.iter().find(|p| p.id == id).map(|p| p.env),
    };
    match known {
        Some("") => None,
        Some(v) => Some(v.to_owned()),
        None => Some(format!(
            "HK_PROVIDER_{}_KEY",
            id.to_ascii_uppercase().replace('-', "_")
        )),
    }
}

/// Key from the environment for `id` (Gemini also honours `GOOGLE_API_KEY`).
pub fn env_key(id: &str) -> Option<String> {
    let read = |v: &str| {
        std::env::var(v)
            .ok()
            .map(|k| k.trim().to_owned())
            .filter(|k| !k.is_empty())
    };
    let var = env_key_var(id)?;
    read(&var).or_else(|| (id == "gemini").then(|| read("GOOGLE_API_KEY")).flatten())
}

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

/// Create/update a provider. Keys go through the hub's `set_api_key`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInput {
    pub id: String,
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
    /// `from_env`: providers whose key sits in the environment are added and, on a fresh install,
    /// enabled (the daemon's way of being configured without a UI).
    pub fn load(raw: Option<&str>, from_env: bool) -> Result<Self, ProvidersError> {
        let fresh = raw.is_none();
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
                        kind: (*kind).to_owned(),
                        base_url: None,
                        // A fresh install starts with the one provider `hk chat` also defaults to.
                        enabled: *id == "deepseek",
                        models: Vec::new(),
                    },
                );
            }
        }
        if from_env {
            for p in PRESETS {
                if !p.env.is_empty()
                    && env_key(p.id).is_some()
                    && !s.providers.iter().any(|x| x.id == p.id)
                {
                    s.providers.push(StoredProvider {
                        id: p.id.to_owned(),
                        kind: p.kind.to_owned(),
                        base_url: Some(p.base_url.to_owned()),
                        enabled: true,
                        models: Vec::new(),
                    });
                }
            }
            if fresh {
                for p in &mut s.providers {
                    if env_key(&p.id).is_some() {
                        p.enabled = true;
                    }
                }
            }
            // `OLLAMA_HOST` is the variable Ollama itself uses.
            if let Ok(h) = std::env::var("OLLAMA_HOST") {
                let h = h.trim().trim_end_matches('/');
                if !h.is_empty()
                    && let Some(p) = s.providers.iter_mut().find(|p| p.id == "ollama")
                    && p.base_url.is_none()
                {
                    p.base_url = Some(if h.contains("://") {
                        h.to_owned()
                    } else {
                        format!("http://{h}")
                    });
                    p.enabled = p.enabled || fresh;
                }
            }
        }
        Ok(s)
    }

    pub fn is_plan(&self) -> bool {
        self.mode.as_deref() == Some("plan")
    }

    pub fn set_plan(&mut self, plan: bool) {
        self.mode = Some(if plan { "plan" } else { "build" }.to_owned());
    }

    /// Insert or update from user input, after validation.
    pub fn upsert(&mut self, input: ProviderInput) -> Result<(), ProvidersError> {
        validate_id(&input.id)?;
        let kind = ProviderKind::parse(&input.kind)
            .ok_or_else(|| ProvidersError::invalid(format!("unknown kind `{}`", input.kind)))?;
        let kind_s = kind_str(kind);
        let builtin = is_builtin(&input.id);
        if builtin
            && BUILTIN
                .iter()
                .any(|(id, k)| *id == input.id && *k != kind_s)
        {
            return Err(ProvidersError::invalid(format!(
                "provider `{}` is a built-in of another kind",
                input.id
            )));
        }
        if !builtin && !matches!(kind, ProviderKind::OpenaiCompat | ProviderKind::Ollama) {
            return Err(ProvidersError::invalid(
                "custom providers must be `openai_compat` or `ollama` (other kinds have fixed ids)",
            ));
        }
        let base_url = input
            .base_url
            .map(|u| u.trim().trim_end_matches('/').to_owned())
            .filter(|u| !u.is_empty());
        if let Some(u) = &base_url
            && !(u.starts_with("http://") || u.starts_with("https://"))
        {
            return Err(ProvidersError::invalid(
                "base_url must start with http:// or https://",
            ));
        }
        if kind == ProviderKind::OpenaiCompat && base_url.is_none() {
            return Err(ProvidersError::invalid(
                "openai_compat providers need a base_url",
            ));
        }
        let models: Vec<String> = input
            .models
            .into_iter()
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty())
            .collect();
        let new = StoredProvider {
            id: input.id,
            kind: kind_s.to_owned(),
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

    pub fn remove(&mut self, id: &str) -> Result<(), ProvidersError> {
        if is_builtin(id) {
            return Err(ProvidersError::Conflict(format!(
                "`{id}` is built in; disable it instead"
            )));
        }
        let before = self.providers.len();
        self.providers.retain(|p| p.id != id);
        if self.providers.len() == before {
            return Err(ProvidersError::NotFound(format!("no provider `{id}`")));
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

fn validate_id(id: &str) -> Result<(), ProvidersError> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(ProvidersError::invalid(
            "provider id must be 1-64 chars of [a-z0-9_-]",
        ))
    }
}

/// Secret-store name holding a provider's API key.
pub fn key_name(provider_id: &str) -> String {
    format!("provider/{provider_id}/api_key")
}

/// Provider id inside a [`key_name`].
pub(crate) fn id_of_key_name(name: &str) -> Option<&str> {
    name.strip_prefix("provider/")?.strip_suffix("/api_key")
}

/// `provider/model` ref check.
pub fn validate_model_ref(m: &str) -> Result<(), ProvidersError> {
    Route::parse(m)
        .map(drop)
        .map_err(|_| ProvidersError::invalid(format!("model must be `provider/model`, got `{m}`")))
}

/// Provider configs for every enabled provider. `has_key` tells whether a key exists: without a
/// key ref, kinds that need one fail with `MissingKey` (surfaced per provider).
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
        capabilities: Capabilities {
            tools: true,
            reasoning: false,
            vision: false,
        },
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

pub fn kind_str(k: ProviderKind) -> &'static str {
    match k {
        ProviderKind::Deepseek => "deepseek",
        ProviderKind::OpenaiCompat => "openai_compat",
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::Gemini => "gemini",
        ProviderKind::Ollama => "ollama",
    }
}

/// Canonical kind string; unknown input maps to `openai_compat` (what the UI offers for custom).
pub fn canonical_kind(s: &str) -> &'static str {
    kind_str(ProviderKind::parse(s).unwrap_or(ProviderKind::OpenaiCompat))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(id: &str, kind: &str, url: Option<&str>) -> ProviderInput {
        ProviderInput {
            id: id.into(),
            kind: kind.into(),
            base_url: url.map(Into::into),
            enabled: true,
            models: vec![],
        }
    }

    #[test]
    fn fresh_install_has_builtins_with_only_deepseek_enabled() {
        let s = Stored::load(None, false).unwrap();
        let on: Vec<_> = s
            .providers
            .iter()
            .filter(|p| p.enabled)
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(on, ["deepseek"]);
        assert_eq!(s.providers.len(), 4);
    }

    #[test]
    fn upsert_validates() {
        let mut s = Stored::load(None, false).unwrap();
        assert!(s.upsert(input("openai", "openai_compat", None)).is_err());
        assert!(s.upsert(input("Bad Id", "ollama", None)).is_err());
        assert!(s.upsert(input("x", "anthropic", None)).is_err());
        assert!(s.upsert(input("deepseek", "ollama", None)).is_err());
        s.upsert(input("lab", "openai", Some("http://h:1/v1/")))
            .unwrap();
        assert_eq!(
            s.find("lab").unwrap().base_url.as_deref(),
            Some("http://h:1/v1")
        );
        assert!(s.remove("deepseek").is_err());
        s.remove("lab").unwrap();
        assert!(s.remove("lab").is_err());
    }

    #[test]
    fn key_name_roundtrips() {
        assert_eq!(id_of_key_name(&key_name("my-lab")), Some("my-lab"));
        assert_eq!(id_of_key_name("other"), None);
    }

    #[test]
    fn env_var_convention() {
        assert_eq!(
            env_key_var("anthropic").as_deref(),
            Some("ANTHROPIC_API_KEY")
        );
        assert_eq!(env_key_var("lmstudio"), None);
        assert_eq!(
            env_key_var("my-lab").as_deref(),
            Some("HK_PROVIDER_MY_LAB_KEY")
        );
    }
}
