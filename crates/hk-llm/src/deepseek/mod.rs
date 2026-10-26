//! DeepSeek adapter: `deepseek-chat` and `deepseek-reasoner` over the
//! OpenAI-style `/chat/completions` SSE API.
// port: dsh packages/llm (deepseek adapter, llm-retry); wire-extension headers not ported yet

use std::fmt;
use std::time::Duration;

use reqwest::header::HeaderMap;
use serde_json::Value;

// Wire pieces live in `openai`; re-exported so DeepSeek callers keep one path.
use crate::openai::{BodyOptions, Endpoint, Scrubber, build_body_with, default_client};
pub use crate::openai::{ChunkParser, RetryPolicy, SseDecoder, decode_stream, error_from_response};
use crate::{
    CancellationToken, Capabilities, ChatRequest, EventStream, LlmError, ModelCost, ModelInfo,
    Provider, Result,
};

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

/// Build the streaming request body.
///
/// DeepSeek specifics:
/// - `reasoning_content` is sent back only on assistant messages that carry
///   tool calls (the API requires it within a tool-call sub-turn and drops or
///   rejects it elsewhere).
/// - `deepseek-reasoner` ignores sampling params, so they are omitted.
/// - No `seed`, `logit_bias` or JSON Schema; JSON mode only.
/// - `stream_options.include_usage` is always on: usage + cache-hit counters
///   arrive in a trailing chunk.
pub fn build_body(req: &ChatRequest) -> Value {
    // DeepSeek's chat API has no vision input: drop images, leave a note the model can see.
    if req.messages.iter().any(|m| !m.images.is_empty()) {
        let mut r = req.clone();
        for m in &mut r.messages {
            if !m.images.is_empty() {
                m.content.push_str(&format!(
                    "\n[{} image(s) omitted: this model cannot see images]",
                    m.images.len()
                ));
                m.images.clear();
            }
        }
        return build_body_with(&r, &body_options());
    }
    build_body_with(req, &body_options())
}

pub(crate) fn body_options() -> BodyOptions {
    BodyOptions {
        echo_reasoning: true,
        send_reasoning_effort: false,
        no_temperature_models: vec!["deepseek-reasoner".into()],
        // Not on DeepSeek's documented parameter list.
        drop_params: vec!["seed".into(), "logit_bias".into(), "json_schema".into()],
        ..BodyOptions::default()
    }
}

/// DeepSeek provider. Holds the API key in memory only; `Debug` redacts it.
pub struct DeepSeek {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    scrub: Scrubber,
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
        Ok(Self::with_client(default_client()?, api_key))
    }

    /// Use a caller-built HTTP client (proxy, custom TLS, tests).
    pub fn with_client(client: reqwest::Client, api_key: impl Into<String>) -> Self {
        let api_key = api_key.into();
        let mut scrub = Scrubber::default();
        scrub.add(&api_key);
        Self {
            client,
            base_url: DEFAULT_BASE_URL.into(),
            api_key,
            scrub,
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
        let headers = HeaderMap::new();
        let resp = Endpoint {
            client: &self.client,
            provider: Self::ID,
            bearer: Some(&self.api_key),
            headers: &headers,
            scrub: &self.scrub,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, Role, ToolCall};
    use serde_json::json;

    #[test]
    fn reasoner_body_shape() {
        let asst = Message {
            images: Vec::new(),
            role: Role::Assistant,
            content: String::new(),
            reasoning: Some("think".into()),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: json!({"path": "a"}),
            }],
            tool_call_id: None,
        };
        let mut req = ChatRequest::new("deepseek-reasoner", vec![asst]);
        req.temperature = Some(0.3);
        let b = build_body(&req);
        assert!(b.get("temperature").is_none());
        assert_eq!(b["stream_options"]["include_usage"], true);
        assert_eq!(b["messages"][0]["reasoning_content"], "think");
    }
}
