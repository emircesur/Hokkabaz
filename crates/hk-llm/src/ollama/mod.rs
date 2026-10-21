//! Native Ollama adapter: `/api/tags` listing, `/api/pull` progress stream and
//! `/api/chat` NDJSON streaming with tools.
//!
//! Use this instead of [`crate::openai::OpenAiCompat`] on Ollama's `/v1`
//! endpoint when you need model management, `keep_alive`, `num_ctx` or the
//! native `thinking` field.
// port: opencode packages/opencode/src/provider (ollama via openai-compatible), dsh packages/llm (local runtimes)

mod ndjson;
mod parse;
mod request;

use std::fmt;
use std::time::Duration;

use reqwest::header::HeaderMap;
use serde_json::{Value, json};

pub use parse::{ChatParser, PullProgress};
pub use request::{BodyOptions, build_body};

use crate::openai::{Endpoint, RetryPolicy, Scrubber, default_client};
use crate::{
    CancellationToken, Capabilities, ChatRequest, EventStream, LlmError, ModelInfo, Provider,
    Result,
};

/// Stream of [`PullProgress`] updates; the last item of a successful pull has
/// `status == "success"`.
pub type PullStream = futures::stream::BoxStream<'static, Result<PullProgress>>;

/// Default local server address.
pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Ollama provider. `base_url` has no path prefix (`http://host:11434`).
///
/// The optional API key (Ollama Cloud / reverse proxies) is held in memory
/// only, redacted in `Debug` and scrubbed from surfaced errors.
pub struct Ollama {
    id: String,
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    headers: HeaderMap,
    scrub: Scrubber,
    retry: RetryPolicy,
    idle_timeout: Duration,
    body: BodyOptions,
}

impl fmt::Debug for Ollama {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ollama")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl Ollama {
    /// New adapter with provider id `ollama`.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        Ok(Self::with_client(default_client()?, base_url))
    }

    /// Use a caller-built HTTP client (proxy, custom TLS, tests).
    pub fn with_client(client: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            id: "ollama".into(),
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: None,
            headers: HeaderMap::new(),
            scrub: Scrubber::default(),
            retry: RetryPolicy::default(),
            // First token can wait on a cold model load.
            idle_timeout: Duration::from_secs(300),
            body: BodyOptions::default(),
        }
    }

    /// Override the provider id (e.g. `ollama-cloud`).
    #[must_use]
    pub fn id_override(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Bearer token for authenticated deployments.
    #[must_use]
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.scrub.add(&key);
        self.api_key = (!key.is_empty()).then_some(key);
        self
    }

    /// Ollama-specific request knobs (`keep_alive`, `num_ctx`).
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

    /// Download `model` (`qwen3:8b`), streaming progress. Cancelling aborts
    /// the request; Ollama resumes partial layers on the next pull.
    pub async fn pull(&self, model: &str, cancel: CancellationToken) -> Result<PullStream> {
        let body = json!({ "model": model, "stream": true });
        let resp = self
            .endpoint()
            .post_ndjson(
                &format!("{}/api/pull", self.base_url),
                &body,
                &self.retry,
                &cancel,
            )
            .await?;
        Ok(ndjson::decode(
            resp.bytes_stream(),
            parse::PullParser::default(),
            cancel,
            self.idle_timeout,
        ))
    }
}

#[async_trait::async_trait]
impl Provider for Ollama {
    fn id(&self) -> &str {
        &self.id
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        let v: Value = self
            .endpoint()
            .get_json(&format!("{}/api/tags", self.base_url))
            .await?;
        let list = v
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| LlmError::Protocol("/api/tags: missing models array".into()))?;
        // `/api/tags` has no context length or capabilities (that needs a
        // per-model `/api/show`); assume tools, like other generic listings.
        Ok(list
            .iter()
            .filter_map(|m| m.get("model").or_else(|| m.get("name"))?.as_str())
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
        let body = build_body(&req, &self.body);
        let resp = self
            .endpoint()
            .post_ndjson(
                &format!("{}/api/chat", self.base_url),
                &body,
                &self.retry,
                &cancel,
            )
            .await?;
        Ok(ndjson::decode(
            resp.bytes_stream(),
            ChatParser::default(),
            cancel,
            self.idle_timeout,
        ))
    }
}
