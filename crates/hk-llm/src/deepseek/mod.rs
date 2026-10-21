//! DeepSeek adapter: `deepseek-chat` and `deepseek-reasoner` over the
//! OpenAI-style `/chat/completions` SSE API.
// port: dsh packages/llm (deepseek adapter, llm-retry); wire-extension headers not ported yet

mod error;
mod parse;
mod request;
mod retry;
mod sse;

use std::fmt;
use std::time::Duration;

use reqwest::header::{ACCEPT, RETRY_AFTER};

pub use error::error_from_response;
pub use parse::{ChunkParser, decode_stream};
pub use request::build_body;
pub use retry::RetryPolicy;
pub use sse::SseDecoder;

use crate::{
    CancellationToken, Capabilities, ChatRequest, EventStream, LlmError, ModelCost, ModelInfo,
    Provider, Result,
};

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const USER_AGENT: &str = concat!("hokkabaz/", env!("CARGO_PKG_VERSION"));

/// DeepSeek provider. Holds the API key in memory only; `Debug` redacts it.
pub struct DeepSeek {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    retry: RetryPolicy,
    idle_timeout: Duration,
}

impl fmt::Debug for DeepSeek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeepSeek")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl DeepSeek {
    /// Provider id used in model refs (`deepseek/deepseek-chat`).
    pub const ID: &'static str = "deepseek";

    /// New adapter against the public API.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| LlmError::Network(e.to_string()))?;
        Ok(Self::with_client(client, api_key))
    }

    /// Use a caller-built HTTP client (proxy, custom TLS, tests).
    pub fn with_client(client: reqwest::Client, api_key: impl Into<String>) -> Self {
        Self {
            client,
            base_url: DEFAULT_BASE_URL.into(),
            api_key: api_key.into(),
            retry: RetryPolicy::default(),
            idle_timeout: Duration::from_secs(120),
        }
    }

    /// Override the endpoint (custom gateway, mock server).
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

    /// Remove the key from text that may be logged or surfaced.
    fn scrub(&self, s: String) -> String {
        if self.api_key.is_empty() {
            s
        } else {
            s.replace(&self.api_key, "<redacted>")
        }
    }

    async fn send_once(&self, body: &serde_json::Value) -> Result<reqwest::Response> {
        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header(ACCEPT, "text/event-stream")
            .json(body)
            .send()
            .await
            .map_err(|e| LlmError::Network(self.scrub(e.without_url().to_string())))?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let text = resp.text().await.unwrap_or_default();
        Err(self.scrub_err(error_from_response(status, retry_after, &text)))
    }

    /// Providers sometimes echo the key in error text; never let it out.
    fn scrub_err(&self, e: LlmError) -> LlmError {
        match e {
            LlmError::Http {
                status,
                message,
                retry_after,
            } => LlmError::Http {
                status,
                message: self.scrub(message),
                retry_after,
            },
            LlmError::Auth(m) => LlmError::Auth(self.scrub(m)),
            LlmError::ContextLength(m) => LlmError::ContextLength(self.scrub(m)),
            LlmError::InvalidRequest(m) => LlmError::InvalidRequest(self.scrub(m)),
            other => other,
        }
    }

    async fn send_with_retry(
        &self,
        body: &serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response> {
        let mut retry = 0;
        loop {
            let res = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(LlmError::Cancelled),
                r = self.send_once(body) => r,
            };
            match res {
                Err(e) if e.is_retryable() && retry < self.retry.max_retries => {
                    let wait = self.retry.delay(
                        retry,
                        match &e {
                            LlmError::Http { retry_after, .. } => *retry_after,
                            _ => None,
                        },
                    );
                    tracing::warn!(provider = Self::ID, retry, ?wait, error = %e, "retrying request");
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => return Err(LlmError::Cancelled),
                        () = tokio::time::sleep(wait) => {}
                    }
                    retry += 1;
                }
                other => return other,
            }
        }
    }
}

#[async_trait::async_trait]
impl Provider for DeepSeek {
    fn id(&self) -> &str {
        Self::ID
    }

    async fn models(&self) -> Result<Vec<ModelInfo>> {
        // Static list: /models returns ids only, no limits or prices. List
        // prices; the models.dev catalog may override.
        let cost = ModelCost {
            input: 0.28,
            output: 0.42,
            cache_read: Some(0.028),
            cache_write: None,
        };
        let model = |id: &str, name: &str, max_output: u64, reasoning: bool| ModelInfo {
            id: id.into(),
            provider: Self::ID.into(),
            name: name.into(),
            context_window: 128_000,
            max_output: Some(max_output),
            capabilities: Capabilities {
                tools: true,
                reasoning,
                vision: false,
            },
            cost: Some(cost),
        };
        Ok(vec![
            model("deepseek-chat", "DeepSeek Chat", 8_192, false),
            model("deepseek-reasoner", "DeepSeek Reasoner", 65_536, true),
        ])
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream> {
        if self.api_key.is_empty() {
            return Err(LlmError::Auth("missing DeepSeek API key".into()));
        }
        let body = build_body(&req);
        let resp = self.send_with_retry(&body, &cancel).await?;
        Ok(decode_stream(
            resp.bytes_stream(),
            cancel,
            self.idle_timeout,
        ))
    }
}
