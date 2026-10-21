//! Model catalog: models.dev JSON -> [`ModelInfo`], file cache, custom endpoints.
//!
//! Network access is isolated in [`CatalogStore::refresh`]; everything else
//! works on text so tests run from recorded fixtures.
// port: opencode packages/opencode/src/provider/models.ts (models.dev schema + refresh)

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::error::{LlmError, Result};
use crate::model::{Capabilities, ModelCost, ModelInfo};

/// Public models.dev dump.
pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";
/// Design §3.4: refreshed daily.
pub const DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Provider-level metadata from the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderInfo {
    /// Provider id, e.g. `deepseek`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// OpenAI-compatible base URL, when the catalog knows one.
    pub api: Option<String>,
    /// Env var names that conventionally hold the API key.
    pub env: Vec<String>,
    /// True for user-defined endpoints (not from models.dev).
    pub custom: bool,
}

/// User-defined model on a custom endpoint. Everything but `id` is optional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomModel {
    /// Model id sent on the wire.
    pub id: String,
    /// Display name; defaults to `id`.
    #[serde(default)]
    pub name: Option<String>,
    /// Context window in tokens; 0 when unknown.
    #[serde(default)]
    pub context_window: u64,
    /// Max output tokens.
    #[serde(default)]
    pub max_output: Option<u64>,
    /// Feature flags.
    #[serde(default)]
    pub capabilities: Capabilities,
    /// Pricing, if any.
    #[serde(default)]
    pub cost: Option<ModelCost>,
}

/// User-defined provider endpoint (vLLM, LM Studio, corporate gateway, ...).
/// Same id as a catalog provider replaces it wholesale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomEndpoint {
    /// Provider id used in `provider/model` references.
    pub id: String,
    /// Display name; defaults to `id`.
    #[serde(default)]
    pub name: Option<String>,
    /// Base URL of the endpoint.
    pub base_url: String,
    /// Env var holding the API key. Keys themselves never live in config.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Models served by the endpoint.
    #[serde(default)]
    pub models: Vec<CustomModel>,
}

/// Loaded catalog: providers plus their models.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    providers: BTreeMap<String, ProviderInfo>,
    models: BTreeMap<(String, String), ModelInfo>,
}

// Wire shapes: lenient, every field defaulted, unknown fields ignored.
#[derive(Deserialize)]
struct WireProvider {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    api: Option<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    models: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct WireModel {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    modalities: WireModalities,
    #[serde(default)]
    cost: Option<WireCost>,
    #[serde(default)]
    limit: WireLimit,
}

#[derive(Deserialize, Default)]
struct WireModalities {
    #[serde(default)]
    input: Vec<String>,
}

#[derive(Deserialize)]
struct WireCost {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    #[serde(default)]
    cache_read: Option<f64>,
    #[serde(default)]
    cache_write: Option<f64>,
}

#[derive(Deserialize, Default)]
struct WireLimit {
    #[serde(default)]
    context: u64,
    #[serde(default)]
    output: Option<u64>,
}

impl Catalog {
    /// Parse a models.dev `api.json` document. Malformed provider or model
    /// entries are skipped (the dump is third-party and changes shape); only a
    /// non-object top level is an error.
    pub fn from_models_dev(json: &str) -> Result<Self> {
        let top: BTreeMap<String, serde_json::Value> = serde_json::from_str(json)
            .map_err(|e| LlmError::Protocol(format!("models.dev: {e}")))?;
        let mut cat = Self::default();
        for (key, value) in top {
            let Ok(p) = serde_json::from_value::<WireProvider>(value) else {
                tracing::warn!(provider = %key, "models.dev: skipping malformed provider");
                continue;
            };
            let pid = p.id.unwrap_or(key);
            for (mkey, mvalue) in p.models {
                let Ok(m) = serde_json::from_value::<WireModel>(mvalue) else {
                    tracing::warn!(provider = %pid, model = %mkey, "models.dev: skipping malformed model");
                    continue;
                };
                let id = m.id.unwrap_or(mkey);
                let info = ModelInfo {
                    name: m.name.unwrap_or_else(|| id.clone()),
                    provider: pid.clone(),
                    context_window: m.limit.context,
                    max_output: m.limit.output,
                    capabilities: Capabilities {
                        tools: m.tool_call,
                        reasoning: m.reasoning,
                        vision: m.modalities.input.iter().any(|s| s == "image"),
                    },
                    cost: m.cost.map(|c| ModelCost {
                        input: c.input,
                        output: c.output,
                        cache_read: c.cache_read,
                        cache_write: c.cache_write,
                    }),
                    id: id.clone(),
                };
                cat.models.insert((pid.clone(), id), info);
            }
            cat.providers.insert(
                pid.clone(),
                ProviderInfo {
                    name: p.name.unwrap_or_else(|| pid.clone()),
                    id: pid,
                    api: p.api,
                    env: p.env,
                    custom: false,
                },
            );
        }
        Ok(cat)
    }

    /// Add or replace a provider with a user-defined endpoint.
    pub fn add_custom(&mut self, ep: &CustomEndpoint) {
        self.models.retain(|(p, _), _| p != &ep.id);
        for m in &ep.models {
            self.models.insert(
                (ep.id.clone(), m.id.clone()),
                ModelInfo {
                    id: m.id.clone(),
                    provider: ep.id.clone(),
                    name: m.name.clone().unwrap_or_else(|| m.id.clone()),
                    context_window: m.context_window,
                    max_output: m.max_output,
                    capabilities: m.capabilities,
                    cost: m.cost,
                },
            );
        }
        self.providers.insert(
            ep.id.clone(),
            ProviderInfo {
                id: ep.id.clone(),
                name: ep.name.clone().unwrap_or_else(|| ep.id.clone()),
                api: Some(ep.base_url.clone()),
                env: ep.api_key_env.iter().cloned().collect(),
                custom: true,
            },
        );
    }

    /// Look up one model.
    pub fn model(&self, provider: &str, id: &str) -> Option<&ModelInfo> {
        self.models.get(&(provider.to_owned(), id.to_owned()))
    }

    /// Look up one provider.
    pub fn provider(&self, id: &str) -> Option<&ProviderInfo> {
        self.providers.get(id)
    }

    /// All providers, sorted by id.
    pub fn providers(&self) -> impl Iterator<Item = &ProviderInfo> {
        self.providers.values()
    }

    /// All models, sorted by (provider, id).
    pub fn models(&self) -> impl Iterator<Item = &ModelInfo> {
        self.models.values()
    }

    /// Models of one provider, sorted by id.
    pub fn models_for<'a>(&'a self, provider: &'a str) -> impl Iterator<Item = &'a ModelInfo> {
        self.models.values().filter(move |m| m.provider == provider)
    }
}

/// Plain-file cache of the raw models.dev document plus fetch logic.
/// The raw text is cached (not the parsed form) so parser fixes apply to old caches.
#[derive(Debug, Clone)]
pub struct CatalogStore {
    path: PathBuf,
    url: String,
    ttl: Duration,
}

impl CatalogStore {
    /// Cache file at `path`, default URL and TTL.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            url: MODELS_DEV_URL.to_owned(),
            ttl: DEFAULT_TTL,
        }
    }

    /// Override the source URL (mirrors, tests).
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Override the freshness window.
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Cached catalog if the file exists and parses, regardless of age.
    pub fn load_cached(&self) -> Option<Catalog> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        Catalog::from_models_dev(&text).ok()
    }

    /// Whether the cache file is younger than the TTL.
    pub fn is_fresh(&self) -> bool {
        std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < self.ttl)
    }

    /// Fetch, validate, and cache. Cache is only replaced by a document that parses.
    pub async fn refresh(&self, client: &reqwest::Client) -> Result<Catalog> {
        let resp = client
            .get(&self.url)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(LlmError::Http {
                status: status.as_u16(),
                message: "models.dev fetch failed".to_owned(),
                retry_after: None,
            });
        }
        let text = resp
            .text()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        let cat = Catalog::from_models_dev(&text)?;
        if let Err(e) = self.write_cache(&text) {
            // Cache is an optimisation; a read-only dir must not break model listing.
            tracing::warn!(path = %self.path.display(), error = %e, "catalog cache write failed");
        }
        Ok(cat)
    }

    /// Fresh cache, else fetch; on fetch failure fall back to a stale cache.
    pub async fn load(&self, client: &reqwest::Client) -> Result<Catalog> {
        if self.is_fresh() {
            if let Some(c) = self.load_cached() {
                return Ok(c);
            }
        }
        match self.refresh(client).await {
            Ok(c) => Ok(c),
            Err(e) => self.load_cached().ok_or(e),
        }
    }

    fn write_cache(&self, text: &str) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Rename so a crash mid-write never leaves a truncated cache.
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/models_dev.json");

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hk-llm-catalog-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn catalog_parses_fixture() {
        let cat = Catalog::from_models_dev(FIXTURE).unwrap();
        let chat = cat.model("deepseek", "deepseek-chat").unwrap();
        assert_eq!(chat.context_window, 64000);
        assert_eq!(chat.max_output, Some(8192));
        assert_eq!(
            chat.capabilities,
            Capabilities {
                tools: true,
                reasoning: false,
                vision: false
            }
        );
        let cost = chat.cost.unwrap();
        assert_eq!(
            (cost.input, cost.output, cost.cache_read, cost.cache_write),
            (0.27, 1.1, Some(0.07), None)
        );
        assert!(
            cat.model("deepseek", "deepseek-reasoner")
                .unwrap()
                .capabilities
                .reasoning
        );

        let sonnet = cat.model("anthropic", "claude-sonnet-4").unwrap();
        assert!(sonnet.capabilities.vision && sonnet.capabilities.tools);
        assert_eq!(sonnet.cost.unwrap().cache_write, Some(3.75));

        let p = cat.provider("deepseek").unwrap();
        assert_eq!(p.api.as_deref(), Some("https://api.deepseek.com"));
        assert_eq!(p.env, ["DEEPSEEK_API_KEY"]);
        assert!(!p.custom);
    }

    #[test]
    fn catalog_tolerates_sparse_and_malformed_entries() {
        let cat = Catalog::from_models_dev(FIXTURE).unwrap();
        let m = cat.model("ollama-cloud", "no-limits").unwrap();
        assert_eq!((m.context_window, m.max_output, m.cost), (0, None, None));
        assert!(cat.model("ollama-cloud", "broken").is_none());
        assert_eq!(cat.models_for("deepseek").count(), 2);
        assert!(Catalog::from_models_dev("[1]").is_err());
    }

    #[test]
    fn catalog_custom_endpoint_added_and_overrides() {
        let mut cat = Catalog::from_models_dev(FIXTURE).unwrap();
        let ep: CustomEndpoint = serde_json::from_str(
            r#"{"id":"lmstudio","base_url":"http://localhost:1234/v1",
                "models":[{"id":"qwen","context_window":32768,"capabilities":{"tools":true,"reasoning":false,"vision":false}}]}"#,
        )
        .unwrap();
        cat.add_custom(&ep);
        let m = cat.model("lmstudio", "qwen").unwrap();
        assert_eq!(
            (m.name.as_str(), m.context_window, m.cost),
            ("qwen", 32768, None)
        );
        assert!(cat.provider("lmstudio").unwrap().custom);

        // Same id as catalog provider replaces its models.
        let over = CustomEndpoint {
            id: "deepseek".into(),
            name: None,
            base_url: "https://gw.example/v1".into(),
            api_key_env: Some("GW_KEY".into()),
            models: vec![],
        };
        cat.add_custom(&over);
        assert_eq!(cat.models_for("deepseek").count(), 0);
        assert_eq!(cat.provider("deepseek").unwrap().env, ["GW_KEY"]);
    }

    #[test]
    fn catalog_cache_roundtrip_and_ttl() {
        let dir = scratch("cache");
        let store = CatalogStore::new(dir.join("sub/models.json"));
        assert!(store.load_cached().is_none() && !store.is_fresh());
        store.write_cache(FIXTURE).unwrap();
        assert!(store.is_fresh());
        assert!(
            store
                .load_cached()
                .unwrap()
                .model("deepseek", "deepseek-chat")
                .is_some()
        );
        assert!(!store.clone().with_ttl(Duration::ZERO).is_fresh());
        let _ = std::fs::remove_dir_all(dir);
    }

    async fn serve_once(body: &'static str, status: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf).await;
            let head = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            s.write_all(head.as_bytes()).await.unwrap();
            s.write_all(body.as_bytes()).await.unwrap();
        });
        format!("http://{addr}/api.json")
    }

    #[tokio::test]
    async fn catalog_refresh_writes_cache_and_stale_fallback() {
        let dir = scratch("refresh");
        let path = dir.join("models.json");
        let client = reqwest::Client::new();

        let url = serve_once(FIXTURE, "200 OK").await;
        let store = CatalogStore::new(&path).with_url(url);
        assert!(
            store
                .load(&client)
                .await
                .unwrap()
                .model("anthropic", "claude-sonnet-4")
                .is_some()
        );
        assert!(store.is_fresh());

        // Stale cache + dead server -> stale data, not an error.
        let url = serve_once("oops", "500 Internal Server Error").await;
        let stale = CatalogStore::new(&path)
            .with_url(url)
            .with_ttl(Duration::ZERO);
        assert!(
            stale
                .load(&client)
                .await
                .unwrap()
                .model("deepseek", "deepseek-chat")
                .is_some()
        );

        // No cache + failure -> error; bad body never replaces cache.
        let url = serve_once("not json", "200 OK").await;
        let fresh = CatalogStore::new(dir.join("none.json")).with_url(url);
        assert!(fresh.load(&client).await.is_err());
        assert!(!dir.join("none.json").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
