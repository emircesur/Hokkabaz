//! Provider registry: plain config -> `Arc<dyn Provider>` instances, plus
//! router assembly and a merged model listing.
//!
//! No dependency on the secret store: callers implement [`KeyResolver`] over
//! `hk-store::secrets`, env vars, or a keychain. Config structs carry only key
//! *references*, never key material.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::anthropic::Anthropic;
use crate::catalog::{Catalog, CustomEndpoint};
use crate::deepseek::DeepSeek;
use crate::gemini::Gemini;
use crate::ollama::{self, Ollama};
use crate::openai::{BodyOptions, OpenAiCompat, RetryPolicy, default_client};
use crate::{LlmError, ModelInfo, Provider, Router};

/// Registry failure. Typed so UIs can point at the offending provider.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The provider needs an API key and the resolver has none.
    #[error("missing API key for provider `{provider}`")]
    MissingKey {
        /// Provider id from the config.
        provider: String,
    },
    /// `kind` is not one of the supported adapter kinds.
    #[error("unknown provider kind `{kind}` for provider `{provider}`")]
    UnknownKind {
        /// Provider id from the config.
        provider: String,
        /// The unrecognised kind string.
        kind: String,
    },
    /// Kind requires `base_url` (openai-compat) and none was given.
    #[error("provider `{provider}` needs a base_url")]
    MissingBaseUrl {
        /// Provider id from the config.
        provider: String,
    },
    /// Two configs share an id.
    #[error("duplicate provider id `{provider}`")]
    Duplicate {
        /// The repeated id.
        provider: String,
    },
    /// Adapter construction or router wiring failed.
    #[error("provider `{provider}`: {source}")]
    Build {
        /// Provider id (or route key) involved.
        provider: String,
        /// Underlying failure.
        #[source]
        source: LlmError,
    },
}

/// Adapter kind a [`ProviderConfig`] selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// DeepSeek public API (fixed provider id `deepseek` unless overridden).
    Deepseek,
    /// Any OpenAI-compatible endpoint; needs `base_url`.
    OpenaiCompat,
    /// Anthropic Messages API.
    Anthropic,
    /// Google Gemini API.
    Gemini,
    /// Ollama native API (local by default).
    Ollama,
}

impl ProviderKind {
    /// Parse the config string. Accepts `-` or `_` and a few common aliases.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "deepseek" => Some(Self::Deepseek),
            "openai_compat" | "openai" | "openai_compatible" => Some(Self::OpenaiCompat),
            "anthropic" => Some(Self::Anthropic),
            "gemini" | "google" => Some(Self::Gemini),
            "ollama" => Some(Self::Ollama),
            _ => None,
        }
    }
}

/// Reference to a secret, resolved by [`KeyResolver`]. An env var name or a
/// secret-store handle; never the key itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyRef(pub String);

impl KeyRef {
    /// New reference.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// Looks up key material for a [`KeyRef`]. Implement over env / secret store.
///
/// Sync on purpose: stores are expected to be cached or in-process. Returned
/// strings are used to build the adapter and not retained by the registry.
pub trait KeyResolver: Send + Sync {
    /// Key for `key_ref`, or `None` when unset/empty.
    fn resolve(&self, key_ref: &KeyRef) -> Option<String>;
}

impl<F> KeyResolver for F
where
    F: Fn(&KeyRef) -> Option<String> + Send + Sync,
{
    fn resolve(&self, key_ref: &KeyRef) -> Option<String> {
        self(key_ref)
    }
}

/// Optional per-provider tuning. All fields default to adapter defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderOptions {
    /// Static extra request headers (openai-compat only).
    pub headers: BTreeMap<String, String>,
    /// Fixed model list instead of querying the provider (openai-compat,
    /// anthropic, gemini).
    pub models: Vec<ModelInfo>,
    /// Connect-phase retries after the first attempt.
    pub max_retries: Option<u32>,
    /// Max silence between stream chunks, seconds.
    pub idle_timeout_secs: Option<u64>,
    /// openai-compat: replay `reasoning_content` on tool-call turns.
    pub echo_reasoning: Option<bool>,
    /// openai-compat: JSON field for the output cap.
    pub max_tokens_field: Option<String>,
    /// openai-compat: send `stream_options.include_usage`.
    pub include_usage: Option<bool>,
    /// openai-compat: send `top_k` / `min_p` / `repetition_penalty`
    /// (vLLM, llama.cpp server, LM Studio).
    pub extended_sampling: Option<bool>,
}

/// Plain description of one provider instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider id used in `provider/model` refs. For `deepseek`, `anthropic`
    /// and `gemini` it must match the adapter's fixed id (those adapters do
    /// not support renaming).
    pub id: String,
    /// Adapter kind string, see [`ProviderKind::parse`].
    pub kind: String,
    /// Endpoint override. Required for `openai_compat`.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Key reference. `None` = no key (allowed for openai-compat and ollama only).
    #[serde(default)]
    pub api_key: Option<KeyRef>,
    /// Tuning.
    #[serde(default)]
    pub options: ProviderOptions,
}

impl ProviderConfig {
    /// Config for a named custom endpoint from the catalog layer. Its
    /// configured models become the fixed model list.
    pub fn from_custom(ep: &CustomEndpoint) -> Self {
        let mut tmp = Catalog::default();
        tmp.add_custom(ep);
        Self {
            id: ep.id.clone(),
            kind: "openai_compat".into(),
            base_url: Some(ep.base_url.clone()),
            api_key: ep.api_key_env.clone().map(KeyRef),
            options: ProviderOptions {
                models: tmp.models_for(&ep.id).cloned().collect(),
                ..ProviderOptions::default()
            },
        }
    }
}

/// Set of built providers, keyed by id.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, Arc<dyn Provider>>,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.providers.keys()).finish()
    }
}

impl ProviderRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build every config; first error aborts.
    pub fn build(
        configs: &[ProviderConfig],
        keys: &dyn KeyResolver,
    ) -> Result<Self, RegistryError> {
        let (reg, mut errs) = Self::build_lenient(configs, keys);
        if errs.is_empty() {
            Ok(reg)
        } else {
            Err(errs.remove(0))
        }
    }

    /// Build every config, collecting failures: one bad provider (e.g. a
    /// missing key) must not take the others down.
    pub fn build_lenient(
        configs: &[ProviderConfig],
        keys: &dyn KeyResolver,
    ) -> (Self, Vec<RegistryError>) {
        let mut reg = Self::new();
        let mut errs = Vec::new();
        for cfg in configs {
            if reg.providers.contains_key(&cfg.id) {
                errs.push(RegistryError::Duplicate {
                    provider: cfg.id.clone(),
                });
                continue;
            }
            match build_provider(cfg, keys) {
                Ok(p) => {
                    reg.providers.insert(cfg.id.clone(), p);
                }
                Err(e) => errs.push(e),
            }
        }
        (reg, errs)
    }

    /// Add a prebuilt provider (tests, plugins), replacing any with the same id.
    #[must_use]
    pub fn with_provider(mut self, p: Arc<dyn Provider>) -> Self {
        self.providers.insert(p.id().to_owned(), p);
        self
    }

    /// Provider by id.
    pub fn get(&self, id: &str) -> Option<&Arc<dyn Provider>> {
        self.providers.get(id)
    }

    /// Registered ids, sorted.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }

    /// Registered providers, sorted by id.
    pub fn providers(&self) -> impl Iterator<Item = &Arc<dyn Provider>> {
        self.providers.values()
    }

    /// Number of providers.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// True when no provider is registered.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Every provider's models, enriched from `catalog` and sorted by
    /// (provider, id). Provider failures are reported, not fatal.
    ///
    /// Merge rule per model: catalog supplies capabilities and fills what the
    /// adapter left unknown (name == id, window 0, no cap, no price). Catalog
    /// models the adapter did not list are added, so offline providers still
    /// show their known models.
    pub async fn list_models(&self, catalog: Option<&Catalog>) -> ModelList {
        let mut out = ModelList::default();
        for (id, p) in &self.providers {
            let listed = match p.models().await {
                Ok(m) => m,
                Err(e) => {
                    out.errors.push((id.clone(), e));
                    Vec::new()
                }
            };
            let mut seen = std::collections::BTreeSet::new();
            for m in listed {
                seen.insert(m.id.clone());
                out.models
                    .push(match catalog.and_then(|c| c.model(id, &m.id)) {
                        Some(c) => enrich(m, c),
                        None => m,
                    });
            }
            if let Some(c) = catalog {
                out.models
                    .extend(c.models_for(id).filter(|m| !seen.contains(&m.id)).cloned());
            }
        }
        out.models
            .sort_by(|a, b| (&a.provider, &a.id).cmp(&(&b.provider, &b.id)));
        out
    }
}

/// Result of [`ProviderRegistry::list_models`].
#[derive(Debug, Default)]
pub struct ModelList {
    /// Merged models.
    pub models: Vec<ModelInfo>,
    /// Providers whose listing failed, with the cause.
    pub errors: Vec<(String, LlmError)>,
}

fn enrich(adapter: ModelInfo, cat: &ModelInfo) -> ModelInfo {
    ModelInfo {
        name: if adapter.name == adapter.id {
            cat.name.clone()
        } else {
            adapter.name
        },
        context_window: if adapter.context_window == 0 {
            cat.context_window
        } else {
            adapter.context_window
        },
        max_output: adapter.max_output.or(cat.max_output),
        // Adapters guess capabilities from bare id lists; catalog knows better.
        capabilities: cat.capabilities,
        cost: adapter.cost.or(cat.cost),
        ..adapter
    }
}

/// One router chain. `key: None` sets the default chain; otherwise the chain
/// is addressable by `key` (e.g. an agent name). Refs are `provider/model`,
/// first = primary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteConfig {
    /// Chain key, or `None` for the default chain.
    #[serde(default)]
    pub key: Option<String>,
    /// Fallback chain of `provider/model` refs.
    pub refs: Vec<String>,
}

/// Router over every provider in `registry` with the given chains.
/// Callers add `.catalog(..)`, retry policy etc. on the result.
pub fn build_router(
    registry: &ProviderRegistry,
    routes: Vec<RouteConfig>,
) -> Result<Router, RegistryError> {
    let mut router = registry
        .providers()
        .fold(Router::new(), |r, p| r.provider(Arc::clone(p)));
    for rc in routes {
        let name = rc.key.clone().unwrap_or_else(|| "default".into());
        let wrap = |source| RegistryError::Build {
            provider: name.clone(),
            source,
        };
        router = match &rc.key {
            Some(k) => router.chain(k, &rc.refs),
            None => router.default_chain(&rc.refs),
        }
        .map_err(wrap)?;
    }
    Ok(router)
}

fn build_provider(
    cfg: &ProviderConfig,
    keys: &dyn KeyResolver,
) -> Result<Arc<dyn Provider>, RegistryError> {
    let id = cfg.id.as_str();
    let kind = ProviderKind::parse(&cfg.kind).ok_or_else(|| RegistryError::UnknownKind {
        provider: id.to_owned(),
        kind: cfg.kind.clone(),
    })?;
    let build_err = |source| RegistryError::Build {
        provider: id.to_owned(),
        source,
    };
    let missing = || RegistryError::MissingKey {
        provider: id.to_owned(),
    };
    // Resolve lazily per kind: `Some(ref)` that resolves to nothing is always
    // an error; `None` is an error only for kinds that require a key.
    let key = |required: bool| -> Result<Option<String>, RegistryError> {
        match &cfg.api_key {
            Some(r) => keys
                .resolve(r)
                .filter(|k| !k.is_empty())
                .map(Some)
                .ok_or_else(missing),
            None if required => Err(missing()),
            None => Ok(None),
        }
    };
    let o = &cfg.options;
    let retry = o.max_retries.map(|n| RetryPolicy {
        max_retries: n,
        ..RetryPolicy::default()
    });
    let idle = o.idle_timeout_secs.map(Duration::from_secs);
    let fixed_id = |want: &str| {
        if id == want {
            Ok(())
        } else {
            Err(build_err(LlmError::InvalidRequest(format!(
                "kind `{}` has fixed provider id `{want}`",
                cfg.kind
            ))))
        }
    };
    let client = default_client().map_err(build_err)?;

    let p: Arc<dyn Provider> = match kind {
        ProviderKind::Deepseek => {
            fixed_id(DeepSeek::ID)?;
            let mut p = DeepSeek::with_client(client, key(true)?.unwrap_or_default());
            if let Some(u) = &cfg.base_url {
                p = p.base_url(u);
            }
            if let Some(r) = retry {
                p = p.retry(r);
            }
            if let Some(d) = idle {
                p = p.idle_timeout(d);
            }
            Arc::new(p)
        }
        ProviderKind::Anthropic => {
            fixed_id(Anthropic::ID)?;
            let mut p = Anthropic::with_client(client, key(true)?.unwrap_or_default());
            if let Some(u) = &cfg.base_url {
                p = p.base_url(u);
            }
            if let Some(r) = retry {
                p = p.retry(r);
            }
            if let Some(d) = idle {
                p = p.idle_timeout(d);
            }
            if !o.models.is_empty() {
                p = p.models_list(o.models.clone());
            }
            Arc::new(p)
        }
        ProviderKind::Gemini => {
            fixed_id(Gemini::ID)?;
            let mut p = Gemini::with_client(client, key(true)?.unwrap_or_default());
            if let Some(u) = &cfg.base_url {
                p = p.base_url(u);
            }
            if let Some(r) = retry {
                p = p.retry(r);
            }
            if let Some(d) = idle {
                p = p.idle_timeout(d);
            }
            if !o.models.is_empty() {
                p = p.models_list(o.models.clone());
            }
            Arc::new(p)
        }
        ProviderKind::OpenaiCompat => {
            let url = cfg
                .base_url
                .as_deref()
                .filter(|u| !u.is_empty())
                .ok_or_else(|| RegistryError::MissingBaseUrl {
                    provider: id.to_owned(),
                })?;
            let mut p = OpenAiCompat::with_client(client, id, url);
            if let Some(k) = key(false)? {
                p = p.api_key(k);
            }
            p = p
                .headers(o.headers.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                .map_err(build_err)?;
            let mut body = BodyOptions::default();
            if let Some(v) = o.echo_reasoning {
                body.echo_reasoning = v;
            }
            if let Some(v) = o.include_usage {
                body.include_usage = v;
            }
            if let Some(v) = &o.max_tokens_field {
                body.max_tokens_field.clone_from(v);
            }
            if let Some(v) = o.extended_sampling {
                body.extended_sampling = v;
            }
            p = p.body_options(body);
            if let Some(r) = retry {
                p = p.retry(r);
            }
            if let Some(d) = idle {
                p = p.idle_timeout(d);
            }
            if !o.models.is_empty() {
                p = p.models_list(o.models.clone());
            }
            Arc::new(p)
        }
        ProviderKind::Ollama => {
            let url = cfg
                .base_url
                .clone()
                .unwrap_or_else(|| ollama::DEFAULT_BASE_URL.to_owned());
            let mut p = Ollama::with_client(client, url).id_override(id);
            if let Some(k) = key(false)? {
                p = p.api_key(k);
            }
            if let Some(r) = retry {
                p = p.retry(r);
            }
            if let Some(d) = idle {
                p = p.idle_timeout(d);
            }
            Arc::new(p)
        }
    };
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::CustomModel;
    use crate::{
        CancellationToken, Capabilities, ChatRequest, EventStream, ModelInfo, Result as LlmResult,
    };

    struct Keys(Vec<(&'static str, &'static str)>);
    impl KeyResolver for Keys {
        fn resolve(&self, r: &KeyRef) -> Option<String> {
            self.0
                .iter()
                .find(|(k, _)| *k == r.0)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    fn cfg(id: &str, kind: &str, key: Option<&str>, url: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            id: id.into(),
            kind: kind.into(),
            base_url: url.map(Into::into),
            api_key: key.map(KeyRef::new),
            options: ProviderOptions::default(),
        }
    }

    fn model(provider: &str, id: &str) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            provider: provider.into(),
            name: id.into(),
            context_window: 0,
            max_output: None,
            capabilities: Capabilities {
                tools: true,
                ..Capabilities::default()
            },
            cost: None,
        }
    }

    struct Fake {
        id: &'static str,
        models: LlmResult<Vec<ModelInfo>>,
    }
    #[async_trait::async_trait]
    impl Provider for Fake {
        fn id(&self) -> &str {
            self.id
        }
        async fn models(&self) -> LlmResult<Vec<ModelInfo>> {
            match &self.models {
                Ok(m) => Ok(m.clone()),
                Err(_) => Err(LlmError::Network("down".into())),
            }
        }
        async fn stream(&self, _: ChatRequest, _: CancellationToken) -> LlmResult<EventStream> {
            Err(LlmError::Cancelled)
        }
    }

    #[test]
    fn builds_all_kinds() {
        let keys = Keys(vec![("K", "sk-x")]);
        let cfgs = [
            cfg("deepseek", "deepseek", Some("K"), None),
            cfg("anthropic", "anthropic", Some("K"), None),
            cfg("gemini", "gemini", Some("K"), None),
            cfg(
                "vllm",
                "openai-compat",
                None,
                Some("http://localhost:8000/v1"),
            ),
            cfg("local", "ollama", None, None),
        ];
        let reg = ProviderRegistry::build(&cfgs, &keys).unwrap();
        assert_eq!(
            reg.ids().collect::<Vec<_>>(),
            ["anthropic", "deepseek", "gemini", "local", "vllm"]
        );
        assert_eq!(reg.get("local").unwrap().id(), "local");
    }

    #[test]
    fn missing_key_is_typed() {
        let keys = Keys(vec![]);
        // No ref for a key-requiring kind.
        let e =
            ProviderRegistry::build(&[cfg("deepseek", "deepseek", None, None)], &keys).unwrap_err();
        assert!(matches!(e, RegistryError::MissingKey { provider } if provider == "deepseek"));
        // Ref that resolves to nothing, even for optional-key kinds.
        let e = ProviderRegistry::build(
            &[cfg(
                "gw",
                "openai_compat",
                Some("GW_KEY"),
                Some("http://x/v1"),
            )],
            &keys,
        )
        .unwrap_err();
        assert!(matches!(e, RegistryError::MissingKey { provider } if provider == "gw"));
    }

    #[test]
    fn unknown_kind_and_missing_url() {
        let keys = Keys(vec![]);
        let e =
            ProviderRegistry::build(&[cfg("x", "mistral-native", None, None)], &keys).unwrap_err();
        assert!(matches!(e, RegistryError::UnknownKind { kind, .. } if kind == "mistral-native"));
        let e = ProviderRegistry::build(&[cfg("x", "openai", None, None)], &keys).unwrap_err();
        assert!(matches!(e, RegistryError::MissingBaseUrl { .. }));
    }

    #[test]
    fn lenient_keeps_good_providers_and_rejects_dupes() {
        let keys = Keys(vec![]);
        let cfgs = [
            cfg("anthropic", "anthropic", None, None),
            cfg("local", "ollama", None, None),
            cfg("local", "ollama", None, None),
        ];
        let (reg, errs) = ProviderRegistry::build_lenient(&cfgs, &keys);
        assert_eq!(reg.ids().collect::<Vec<_>>(), ["local"]);
        assert!(matches!(errs[0], RegistryError::MissingKey { .. }));
        assert!(matches!(errs[1], RegistryError::Duplicate { .. }));
    }

    #[test]
    fn closure_resolver_and_fixed_id() {
        let keys = |r: &KeyRef| (r.0 == "K").then(|| "sk".to_owned());
        assert!(
            ProviderRegistry::build(&[cfg("deepseek", "deepseek", Some("K"), None)], &keys).is_ok()
        );
        let e =
            ProviderRegistry::build(&[cfg("ds2", "deepseek", Some("K"), None)], &keys).unwrap_err();
        assert!(matches!(e, RegistryError::Build { .. }));
    }

    #[test]
    fn custom_endpoint_config_roundtrip() {
        let ep = CustomEndpoint {
            id: "corp".into(),
            name: None,
            base_url: "http://gw/v1".into(),
            api_key_env: Some("CORP_KEY".into()),
            models: vec![CustomModel {
                id: "m1".into(),
                name: None,
                context_window: 8000,
                max_output: None,
                capabilities: Capabilities::default(),
                cost: None,
            }],
        };
        let c = ProviderConfig::from_custom(&ep);
        assert_eq!(c.api_key, Some(KeyRef::new("CORP_KEY")));
        assert_eq!(c.options.models.len(), 1);
        let reg = ProviderRegistry::build(&[c], &Keys(vec![("CORP_KEY", "k")])).unwrap();
        assert!(reg.get("corp").is_some());
    }

    #[tokio::test]
    async fn list_models_merges_catalog() {
        let mut cat = Catalog::default();
        cat.add_custom(&CustomEndpoint {
            id: "a".into(),
            name: None,
            base_url: "http://a".into(),
            api_key_env: None,
            models: vec![
                CustomModel {
                    id: "m1".into(),
                    name: Some("Model One".into()),
                    context_window: 32_000,
                    max_output: Some(4096),
                    capabilities: Capabilities {
                        tools: false,
                        reasoning: true,
                        vision: false,
                    },
                    cost: None,
                },
                CustomModel {
                    id: "m3".into(),
                    name: None,
                    context_window: 1000,
                    max_output: None,
                    capabilities: Capabilities::default(),
                    cost: None,
                },
            ],
        });
        let reg = ProviderRegistry::new()
            .with_provider(Arc::new(Fake {
                id: "a",
                models: Ok(vec![model("a", "m1"), model("a", "m2")]),
            }))
            .with_provider(Arc::new(Fake {
                id: "b",
                models: Err(LlmError::Network(String::new())),
            }));
        let list = reg.list_models(Some(&cat)).await;
        let ids: Vec<_> = list.models.iter().map(|m| (&*m.provider, &*m.id)).collect();
        assert_eq!(ids, [("a", "m1"), ("a", "m2"), ("a", "m3")]);
        let m1 = &list.models[0];
        assert_eq!(m1.name, "Model One");
        assert_eq!(m1.context_window, 32_000);
        assert_eq!(m1.max_output, Some(4096));
        assert!(m1.capabilities.reasoning && !m1.capabilities.tools);
        // Not in catalog: adapter data untouched.
        assert_eq!(list.models[1].context_window, 0);
        assert_eq!(list.errors.len(), 1);
        assert_eq!(list.errors[0].0, "b");

        let plain = reg.list_models(None).await;
        assert_eq!(plain.models.len(), 2);
    }

    #[test]
    fn router_from_registry() {
        let reg = ProviderRegistry::new().with_provider(Arc::new(Fake {
            id: "a",
            models: Ok(vec![]),
        }));
        let ok = build_router(
            &reg,
            vec![
                RouteConfig {
                    key: None,
                    refs: vec!["a/m1".into()],
                },
                RouteConfig {
                    key: Some("coder".into()),
                    refs: vec!["a/m2".into(), "a/m3".into()],
                },
            ],
        );
        assert!(ok.is_ok());
        let bad = build_router(
            &reg,
            vec![RouteConfig {
                key: Some("x".into()),
                refs: vec!["nope/m".into()],
            }],
        );
        assert!(matches!(bad, Err(RegistryError::Build { provider, .. }) if provider == "x"));
    }
}
