//! Google Gemini adapter (`generativelanguage.googleapis.com`, v1beta,
//! `models/{model}:streamGenerateContent?alt=sse`).
//!
//! - Auth: `x-goog-api-key` header (never the `?key=` query, which would leak
//!   into URLs and logs); the key is scrubbed from every surfaced error.
//! - Thought parts (`thought: true`, requested via `reasoning_effort`) map to
//!   [`crate::StreamEvent::ReasoningDelta`]. Reasoning text is not replayed.
//! - `thoughtSignature`: opaque token on the `functionCall` part of a tool
//!   turn. Gemini 3 requires it back on replay. The stream vocabulary has no
//!   slot, so it is kept in an in-memory store keyed by tool-call id (same
//!   approach as the Anthropic adapter) and re-attached when the same `Gemini`
//!   instance serves the follow-up. On a miss, `gemini-3*` requests carry
//!   Google's documented bypass value on the first call; 2.x needs nothing.
//!   Persisting signatures in the session log would need an additive
//!   `Message`/`StreamEvent` field.
//! - Usage: `cachedContentTokenCount` -> `cache_read_tokens` (excluded from
//!   `input_tokens`), `thoughtsTokenCount` -> `reasoning_tokens` and included
//!   in `output_tokens`.
// port: opencode packages/opencode/src/provider (google), dsh packages/llm (shared wire plumbing)

pub(crate) mod request;
mod store;
mod stream;

use std::fmt;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;

use crate::openai::{Endpoint, RetryPolicy, Scrubber, decode_stream_with, default_client};
use crate::{
    CancellationToken, Capabilities, ChatRequest, EventStream, LlmError, ModelInfo, Provider,
    Result,
};
use store::SignatureStore;
use stream::GeminiParser;

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Gemini overflow wording: "The input token count (N) exceeds the maximum
/// number of tokens allowed (M)."
fn is_context_overflow(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("exceeds the maximum number of tokens") || m.contains("context length")
}

/// Gemini answers a bad key with 400 INVALID_ARGUMENT, not 401.
fn map_http_error(e: LlmError) -> LlmError {
    match e {
        LlmError::InvalidRequest(m) if m.contains("API key not valid") => LlmError::Auth(m),
        LlmError::InvalidRequest(m) if is_context_overflow(&m) => LlmError::ContextLength(m),
        other => other,
    }
}

/// Gemini provider. Holds the API key in memory only; `Debug` redacts it.
pub struct Gemini {
    client: reqwest::Client,
    base_url: String,
    headers: HeaderMap,
    scrub: Scrubber,
    retry: RetryPolicy,
    idle_timeout: Duration,
    models: Vec<ModelInfo>,
    store: SignatureStore,
}

impl fmt::Debug for Gemini {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gemini")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl Gemini {
    /// Provider id used in model refs (`gemini/gemini-2.5-pro`).
    pub const ID: &'static str = "gemini";

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
        // unusable anyway and the API answers 400 "API key not valid", which
        // surfaces as Auth.
        if let Ok(mut v) = HeaderValue::from_str(&api_key) {
            v.set_sensitive(true);
            headers.insert(HeaderName::from_static("x-goog-api-key"), v);
        }
        Self {
            client,
            base_url: DEFAULT_BASE_URL.into(),
            headers,
            scrub,
            retry: RetryPolicy::default(),
            idle_timeout: Duration::from_secs(120),
            models: Vec::new(),
            store: SignatureStore::default(),
        }
    }

    /// Override the API origin (gateway, tests). No trailing `/v1beta`.
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

    /// Fixed model list. When empty, `models()` queries `GET /v1beta/models`.
    #[must_use]
    pub fn models_list(mut self, models: Vec<ModelInfo>) -> Self {
        self.models = models;
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

/// Build the request body (exposed for log/replay tooling). Starts from an
/// empty signature store, i.e. the first request of a conversation.
pub fn build_body(req: &ChatRequest) -> Value {
    request::build_body(req, &SignatureStore::default())
}

fn model_info(v: &Value) -> Option<ModelInfo> {
    let id = v.get("name")?.as_str()?.strip_prefix("models/")?;
    let methods = v.get("supportedGenerationMethods")?.as_array()?;
    if !methods.iter().any(|m| m == "generateContent") {
        return None;
    }
    Some(ModelInfo {
        id: id.to_owned(),
        provider: Gemini::ID.into(),
        name: v
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_owned(),
        context_window: v
            .get("inputTokenLimit")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        max_output: v.get("outputTokenLimit").and_then(Value::as_u64),
        capabilities: Capabilities {
            tools: true,
            reasoning: v.get("thinking").and_then(Value::as_bool).unwrap_or(false),
            vision: id.starts_with("gemini"),
        },
        cost: None,
    })
}

#[async_trait::async_trait]
impl Provider for Gemini {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        if !self.models.is_empty() {
            return Ok(self.models.clone());
        }
        let v = self
            .endpoint()
            .get_json(&format!("{}/v1beta/models?pageSize=1000", self.base_url))
            .await
            .map_err(map_http_error)?;
        let list = v
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| LlmError::Protocol("/v1beta/models: missing models array".into()))?;
        Ok(list.iter().filter_map(model_info).collect())
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream> {
        let model = req.model.strip_prefix("models/").unwrap_or(&req.model);
        // The id lands in the URL path; refuse anything that could alter it.
        if model.is_empty()
            || !model
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(LlmError::InvalidRequest(format!("bad model id {model:?}")));
        }
        let body = request::build_body(&req, &self.store);
        let resp = self
            .endpoint()
            .post_sse(
                &format!(
                    "{}/v1beta/models/{model}:streamGenerateContent?alt=sse",
                    self.base_url
                ),
                &body,
                &self.retry,
                &cancel,
            )
            .await
            .map_err(map_http_error)?;
        Ok(decode_stream_with(
            resp.bytes_stream(),
            GeminiParser::new(self.store.clone(), self.scrub.clone()),
            cancel,
            self.idle_timeout,
        ))
    }
}
