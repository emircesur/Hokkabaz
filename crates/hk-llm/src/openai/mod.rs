//! OpenAI-compatible `/chat/completions` adapter and the wire pieces shared
//! by every OpenAI-style backend (DeepSeek, OpenRouter, Ollama, vLLM, ...).
//!
//! Later providers either use [`OpenAiCompat`] directly (base URL + headers
//! config) or compose the building blocks: [`SseDecoder`], [`decode_stream`],
//! [`build_body_with`], [`RetryPolicy`], [`error_from_response`].
// port: opencode packages/opencode/src/provider (openai-compatible), dsh packages/llm (wire shared with deepseek)

mod error;
mod http;
mod parse;
mod request;
mod retry;
mod sse;

use std::fmt;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

pub use error::error_from_response;
pub use http::{Endpoint, Scrubber};
pub use parse::{ChunkParser, decode_stream};
pub use request::{BodyOptions, build_body_with};
pub use retry::RetryPolicy;
pub use sse::SseDecoder;

use crate::{
    CancellationToken, Capabilities, ChatRequest, EventStream, LlmError, ModelInfo, Provider,
    Result,
};

const USER_AGENT: &str = concat!("hokkabaz/", env!("CARGO_PKG_VERSION"));

/// Build the default HTTP client used by adapters.
pub fn default_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| LlmError::Network(e.to_string()))
}

/// Generic OpenAI-compatible provider.
///
/// `base_url` includes the API prefix, e.g. `https://api.openai.com/v1` or
/// `http://localhost:11434/v1`; `/chat/completions` and `/models` are appended.
/// The API key and header values are held in memory only, redacted in `Debug`
/// and scrubbed from surfaced errors.
pub struct OpenAiCompat {
    id: String,
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    headers: HeaderMap,
    scrub: Scrubber,
    retry: RetryPolicy,
    idle_timeout: Duration,
    models: Vec<ModelInfo>,
    body: BodyOptions,
}

impl fmt::Debug for OpenAiCompat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompat")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl OpenAiCompat {
    /// New adapter. `id` is the provider id used in model refs (`id/model`).
    pub fn new(id: impl Into<String>, base_url: impl Into<String>) -> Result<Self> {
        Ok(Self::with_client(default_client()?, id, base_url))
    }

    /// Use a caller-built HTTP client (proxy, custom TLS, tests).
    pub fn with_client(
        client: reqwest::Client,
        id: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: None,
            headers: HeaderMap::new(),
            scrub: Scrubber::default(),
            retry: RetryPolicy::default(),
            idle_timeout: Duration::from_secs(120),
            models: Vec::new(),
            body: BodyOptions::default(),
        }
    }

    /// Bearer token (`Authorization: Bearer ...`). Omit for local servers.
    #[must_use]
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.scrub.add(&key);
        self.api_key = (!key.is_empty()).then_some(key);
        self
    }

    /// Add a static request header (`api-key`, `HTTP-Referer`, ...). The
    /// value is treated as secret.
    pub fn header(mut self, name: &str, value: &str) -> Result<Self> {
        let n = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| LlmError::InvalidRequest(format!("bad header name {name:?}")))?;
        // Never echo the value: it may be a credential.
        let mut v = HeaderValue::from_str(value)
            .map_err(|_| LlmError::InvalidRequest(format!("bad value for header {name:?}")))?;
        v.set_sensitive(true);
        self.scrub.add(value);
        self.headers.insert(n, v);
        Ok(self)
    }

    /// Add several headers; see [`Self::header`].
    pub fn headers<K, V>(mut self, it: impl IntoIterator<Item = (K, V)>) -> Result<Self>
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        for (k, v) in it {
            self = self.header(k.as_ref(), v.as_ref())?;
        }
        Ok(self)
    }

    /// Fixed model list. When empty, `models()` queries `GET {base}/models`.
    #[must_use]
    pub fn models_list(mut self, models: Vec<ModelInfo>) -> Self {
        self.models = models;
        self
    }

    /// Override per-backend request-body quirks.
    #[must_use]
    pub fn body_options(mut self, opts: BodyOptions) -> Self {
        self.body = opts;
        self
    }

    /// Override the connect-phase retry policy.
    #[must_use]
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Override the max wait for the next network chunk mid-stream.
    #[must_use]
    pub fn idle_timeout(mut self, d: Duration) -> Self {
        self.idle_timeout = d;
        self
    }

    fn endpoint(&self) -> Endpoint<'_> {
        Endpoint {
            client: &self.client,
            provider: &self.id,
            bearer: self.api_key.as_deref(),
            headers: &self.headers,
            scrub: &self.scrub,
        }
    }
}

#[async_trait::async_trait]
impl Provider for OpenAiCompat {
    fn id(&self) -> &str {
        &self.id
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        if !self.models.is_empty() {
            return Ok(self.models.clone());
        }
        let v = self
            .endpoint()
            .get_json(&format!("{}/models", self.base_url))
            .await?;
        // `/models` carries ids only; limits and prices come from the catalog.
        let list = v
            .get("data")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| LlmError::Protocol("/models: missing data array".into()))?;
        Ok(list
            .iter()
            .filter_map(|m| m.get("id")?.as_str())
            .map(|id| ModelInfo {
                id: id.to_owned(),
                provider: self.id.clone(),
                name: id.to_owned(),
                context_window: 0,
                max_output: None,
                capabilities: Capabilities {
                    tools: true,
                    reasoning: false,
                    vision: false,
                },
                cost: None,
            })
            .collect())
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream> {
        let body = build_body_with(&req, &self.body);
        let resp = self
            .endpoint()
            .post_sse(
                &format!("{}/chat/completions", self.base_url),
                &body,
                &self.retry,
                &cancel,
            )
            .await?;
        Ok(decode_stream(
            resp.bytes_stream(),
            cancel,
            self.idle_timeout,
        ))
    }
}
