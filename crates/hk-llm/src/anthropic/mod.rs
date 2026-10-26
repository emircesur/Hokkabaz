//! Anthropic Messages API adapter (`POST /v1/messages`, SSE streaming).
//!
//! - Thinking maps to [`crate::StreamEvent::ReasoningDelta`]; `reasoning_effort`
//!   opts in (budget dialect for Claude <= 4.5, adaptive for newer models, see
//!   [`ThinkingStyle`]).
//! - Prompt caching: explicit `cache_control` breakpoints on the system prompt,
//!   the last tool and the last message, toggled by `ChatRequest::prompt_cache`
//!   (default: adapter setting, on). Usage maps cache reads/writes to
//!   `cache_read_tokens` / `cache_write_tokens`; `input_tokens` is the
//!   uncached remainder, as Anthropic reports it.
//! - Signed thinking blocks must be replayed on tool turns. The stream
//!   vocabulary and `Message` carry no signature, so the adapter keeps the
//!   blocks in an in-memory store keyed by tool-call id and re-attaches them
//!   when the same `Anthropic` instance serves the follow-up request. On a
//!   miss (restart, resumed session) thinking is switched off for that
//!   request rather than sending an invalid history. Persisting signatures in
//!   the session log would need an additive `Message`/`StreamEvent` field and
//!   matching changes in hk-session/hk-agent.
// port: opencode packages/opencode/src/provider (anthropic), dsh packages/llm (shared wire plumbing)

pub(crate) mod request;
mod store;
mod stream;

use std::fmt;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

pub use request::{RequestOptions, ThinkingStyle};

use crate::openai::{Endpoint, RetryPolicy, Scrubber, decode_stream_with, default_client};
use crate::{
    CancellationToken, Capabilities, ChatRequest, EventStream, LlmError, ModelInfo, Provider,
    Result,
};
use store::ReasoningStore;
use stream::AnthropicParser;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

/// Anthropic provider. Holds the API key in memory only; `Debug` redacts it.
pub struct Anthropic {
    client: reqwest::Client,
    base_url: String,
    headers: HeaderMap,
    scrub: Scrubber,
    retry: RetryPolicy,
    idle_timeout: Duration,
    models: Vec<ModelInfo>,
    opts: RequestOptions,
    store: ReasoningStore,
}

impl fmt::Debug for Anthropic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Anthropic")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("retry", &self.retry)
            .field("opts", &self.opts)
            .finish_non_exhaustive()
    }
}

impl Anthropic {
    /// Provider id used in model refs (`anthropic/claude-sonnet-4-5`).
    pub const ID: &'static str = "anthropic";

    /// New adapter against the public API.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        Ok(Self::with_client(default_client()?, api_key))
    }

    /// Use a caller-built HTTP client (proxy, custom TLS, tests).
    pub fn with_client(client: reqwest::Client, api_key: impl Into<String>) -> Self {
        let api_key = api_key.into();
        let mut scrub = Scrubber::default();
        scrub.add(&api_key);
        let mut headers = HeaderMap::new();
        // Header values can only fail on control chars; a key like that is
        // unusable anyway and the API answers 401, which surfaces as Auth.
        if let Ok(mut v) = HeaderValue::from_str(&api_key) {
            v.set_sensitive(true);
            headers.insert(HeaderName::from_static("x-api-key"), v);
        }
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static(API_VERSION),
        );
        Self {
            client,
            base_url: DEFAULT_BASE_URL.into(),
            headers,
            scrub,
            retry: RetryPolicy::default(),
            idle_timeout: Duration::from_secs(120),
            models: Vec::new(),
            opts: RequestOptions::default(),
            store: ReasoningStore::default(),
        }
    }

    /// Override the API origin (gateway, tests). No trailing `/v1`.
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into().trim_end_matches('/').to_owned();
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

    /// Fixed model list. When empty, `models()` queries `GET /v1/models`.
    #[must_use]
    pub fn models_list(mut self, models: Vec<ModelInfo>) -> Self {
        self.models = models;
        self
    }

    /// Request-building defaults (cache, max tokens, thinking dialect).
    #[must_use]
    pub fn request_options(mut self, opts: RequestOptions) -> Self {
        self.opts = opts;
        self
    }

    fn endpoint(&self) -> Endpoint<'_> {
        Endpoint {
            client: &self.client,
            provider: Self::ID,
            bearer: None,
            headers: &self.headers,
            scrub: &self.scrub,
        }
    }
}

/// Build the streaming request body (exposed for log/replay tooling).
pub fn build_body(req: &ChatRequest, opts: &RequestOptions) -> Value {
    // A fresh store means "no replayable thinking": identical to the first
    // request of a conversation.
    request::build_body(req, opts, &ReasoningStore::default())
}

fn model_info(v: &Value) -> Option<ModelInfo> {
    let id = v.get("id")?.as_str()?;
    let flag = |path: &[&str]| {
        path.iter()
            .try_fold(v.get("capabilities")?, |c, k| c.get(k))
            .and_then(|c| c.get("supported"))
            .and_then(Value::as_bool)
    };
    Some(ModelInfo {
        id: id.to_owned(),
        provider: Anthropic::ID.into(),
        name: v
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_owned(),
        context_window: v
            .get("max_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        max_output: v.get("max_tokens").and_then(Value::as_u64),
        capabilities: Capabilities {
            tools: true,
            reasoning: flag(&["thinking"]).unwrap_or(false),
            vision: flag(&["image_input"]).unwrap_or(false),
        },
        cost: None,
    })
}

#[async_trait::async_trait]
impl Provider for Anthropic {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        if !self.models.is_empty() {
            return Ok(self.models.clone());
        }
        let v = self
            .endpoint()
            .get_json(&format!("{}/v1/models?limit=1000", self.base_url))
            .await?;
        let list = v
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| LlmError::Protocol("/v1/models: missing data array".into()))?;
        Ok(list.iter().filter_map(model_info).collect())
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream> {
        let body = request::build_body(&req, &self.opts, &self.store);
        let resp = self
            .endpoint()
            .post_sse(
                &format!("{}/v1/messages", self.base_url),
                &body,
                &self.retry,
                &cancel,
            )
            .await?;
        Ok(decode_stream_with(
            resp.bytes_stream(),
            AnthropicParser::new(self.store.clone()),
            cancel,
            self.idle_timeout,
        ))
    }
}
