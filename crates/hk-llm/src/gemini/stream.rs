//! Gemini SSE (`alt=sse`) payloads -> [`StreamEvent`]s.
//!
//! Every `data:` payload is a full `GenerateContentResponse` delta. There is
//! no terminal marker: the body just ends, so `Finish` is emitted at EOF (and
//! a body that ended without any `finishReason` / block verdict is a truncated
//! stream, surfaced as a protocol error).
//!
//! Mapping notes:
//! - `thought: true` text parts -> `ReasoningDelta`; plain text -> `TextDelta`.
//! - `functionCall` arrives whole (never argument-streamed on the public API),
//!   so one `ToolCallDelta` carries id + name + complete JSON arguments. The
//!   wire has no call ids; one is synthesised (`gemini_<boot>_<n>`) unless the
//!   model supplies `id`. Its `thoughtSignature`, if any, goes to the
//!   [`SignatureStore`] for replay.
//! - `STOP` with tool calls present is reported as `ToolCalls`; Gemini has no
//!   dedicated tool-use finish reason.
//! - A blocked prompt (`promptFeedback.blockReason`, no candidates) and
//!   candidate-level safety stops (`SAFETY`, `PROHIBITED_CONTENT`, ...) end as
//!   `Finish(ContentFilter)`; the reason is logged, not surfaced as an error,
//!   so the agent can show "blocked" instead of retrying.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::Value;

use super::store::SignatureStore;
use crate::openai::{PayloadParser, Scrubber};
use crate::{FinishReason, LlmError, Result, StreamEvent, Usage};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Chunk {
    #[serde(default)]
    candidates: Vec<Candidate>,
    usage_metadata: Option<WireUsage>,
    prompt_feedback: Option<PromptFeedback>,
    error: Option<WireError>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    content: Option<Content>,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Content {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Part {
    text: Option<String>,
    #[serde(default)]
    thought: bool,
    thought_signature: Option<String>,
    function_call: Option<FnCall>,
}

#[derive(Deserialize)]
struct FnCall {
    id: Option<String>,
    name: String,
    args: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptFeedback {
    block_reason: Option<String>,
}

#[derive(Deserialize, Default, PartialEq, Clone, Copy)]
#[serde(rename_all = "camelCase")]
struct WireUsage {
    #[serde(default)]
    prompt_token_count: u64,
    #[serde(default)]
    cached_content_token_count: u64,
    #[serde(default)]
    candidates_token_count: u64,
    #[serde(default)]
    thoughts_token_count: u64,
    #[serde(default)]
    tool_use_prompt_token_count: u64,
}

impl WireUsage {
    fn into_usage(self) -> Usage {
        // `promptTokenCount` includes the cached part; ours excludes it.
        // Tool-use prompt tokens (built-in tools) are billed as input.
        Usage {
            input_tokens: self
                .prompt_token_count
                .saturating_sub(self.cached_content_token_count)
                + self.tool_use_prompt_token_count,
            // `candidatesTokenCount` excludes thoughts, which are billed as output.
            output_tokens: self.candidates_token_count + self.thoughts_token_count,
            cache_read_tokens: self.cached_content_token_count,
            cache_write_tokens: 0,
            reasoning_tokens: self.thoughts_token_count,
        }
    }
}

#[derive(Deserialize)]
struct WireError {
    #[serde(default)]
    code: u16,
    #[serde(default)]
    message: String,
    #[serde(default)]
    status: String,
}

fn error_event(e: WireError) -> LlmError {
    let m = e.message;
    // Mapped to the HTTP-equivalent so router retry/fallback treats in-stream
    // errors like pre-stream ones.
    let http = |status| LlmError::Http {
        status,
        message: m.clone(),
        retry_after: None,
    };
    match (e.status.as_str(), e.code) {
        ("UNAUTHENTICATED" | "PERMISSION_DENIED", _) | (_, 401 | 403) => LlmError::Auth(m),
        ("INVALID_ARGUMENT" | "FAILED_PRECONDITION", _) | (_, 400) => {
            if super::is_context_overflow(&m) {
                LlmError::ContextLength(m)
            } else {
                LlmError::InvalidRequest(m)
            }
        }
        ("RESOURCE_EXHAUSTED", _) | (_, 429) => http(429),
        ("UNAVAILABLE", _) => http(503),
        ("DEADLINE_EXCEEDED", _) => http(504),
        ("INTERNAL", _) => http(500),
        (_, c @ 500..=599) => http(c),
        _ => LlmError::Protocol(format!("provider error mid-stream: {} {m}", e.status)),
    }
}

fn finish_reason(s: &str, saw_tool_call: bool) -> FinishReason {
    match s {
        "STOP" if saw_tool_call => FinishReason::ToolCalls,
        "STOP" => FinishReason::Stop,
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY"
        | "RECITATION"
        | "BLOCKLIST"
        | "PROHIBITED_CONTENT"
        | "SPII"
        | "IMAGE_SAFETY"
        | "IMAGE_PROHIBITED_CONTENT"
        | "IMAGE_RECITATION" => FinishReason::ContentFilter,
        // MALFORMED_FUNCTION_CALL, UNEXPECTED_TOOL_CALL, OTHER, LANGUAGE, ...
        _ => FinishReason::Other,
    }
}

/// Process-unique prefix so ids synthesised after a restart never collide
/// with ids already in a resumed session log.
fn boot_tag() -> u64 {
    static TAG: OnceLock<u64> = OnceLock::new();
    *TAG.get_or_init(|| {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| {
            u64::try_from(d.as_nanos() & 0xffff_ffff_ffff).unwrap_or(0)
        })
    })
}

fn synth_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "gemini_{:x}_{}",
        boot_tag(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Stateful per-response parser.
pub(crate) struct GeminiParser {
    store: SignatureStore,
    scrub: Scrubber,
    tool_count: u32,
    last_usage: Option<WireUsage>,
    finish: Option<FinishReason>,
    raw_finish: Option<String>,
    done: bool,
}

impl std::fmt::Debug for GeminiParser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiParser")
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl GeminiParser {
    pub(crate) fn new(store: SignatureStore, scrub: Scrubber) -> Self {
        Self {
            store,
            scrub,
            tool_count: 0,
            last_usage: None,
            finish: None,
            raw_finish: None,
            done: false,
        }
    }

    fn part(&mut self, p: Part, out: &mut Vec<StreamEvent>) {
        if let Some(fc) = p.function_call {
            let id = fc.id.filter(|s| !s.is_empty()).unwrap_or_else(synth_id);
            if let Some(sig) = p.thought_signature.as_deref() {
                self.store.put(&id, sig);
            }
            let args = fc.args.unwrap_or_else(|| Value::Object(Default::default()));
            out.push(StreamEvent::ToolCallDelta {
                index: self.tool_count,
                id: Some(id),
                name: Some(fc.name),
                arguments_delta: args.to_string(),
            });
            self.tool_count += 1;
        } else if let Some(text) = p.text.filter(|t| !t.is_empty()) {
            // Signatures on text parts are optional to replay; dropped.
            out.push(if p.thought {
                StreamEvent::ReasoningDelta { text }
            } else {
                StreamEvent::TextDelta { text }
            });
        }
    }

    fn feed_chunk(&mut self, chunk: Chunk) -> Result<Vec<StreamEvent>> {
        if let Some(e) = chunk.error {
            return Err(self.scrub.err(error_event(e)));
        }
        let mut out = Vec::new();
        if let Some(reason) = chunk.prompt_feedback.and_then(|f| f.block_reason) {
            tracing::warn!(reason, "gemini: prompt blocked");
            self.raw_finish = Some(reason);
            self.finish = Some(FinishReason::ContentFilter);
        }
        // n=1 always; extra candidates are ignored.
        if let Some(c) = chunk.candidates.into_iter().next() {
            for p in c.content.unwrap_or_default().parts {
                self.part(p, &mut out);
            }
            if let Some(r) = c.finish_reason {
                // Resolved at EOF: tool calls may still be in later chunks.
                self.raw_finish = Some(r);
                self.finish = None;
            }
        }
        if let Some(u) = chunk.usage_metadata
            && self.last_usage != Some(u)
        {
            self.last_usage = Some(u);
            out.push(StreamEvent::Usage(u.into_usage()));
        }
        Ok(out)
    }

    fn finish_now(&mut self) -> Result<Vec<StreamEvent>> {
        let reason = match (self.finish, self.raw_finish.as_deref()) {
            (Some(r), _) => r,
            (None, Some(raw)) => {
                let r = finish_reason(raw, self.tool_count > 0);
                if r == FinishReason::Other {
                    tracing::warn!(reason = raw, "gemini: unmapped finishReason");
                }
                r
            }
            (None, None) => {
                return Err(LlmError::Protocol(
                    "stream ended without finishReason".into(),
                ));
            }
        };
        self.done = true;
        Ok(vec![StreamEvent::Finish(reason)])
    }
}

impl PayloadParser for GeminiParser {
    fn feed(&mut self, data: &str) -> Result<Vec<StreamEvent>> {
        if self.done {
            return Ok(Vec::new());
        }
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let chunk: Chunk = serde_json::from_str(data)
            .map_err(|e| LlmError::Protocol(format!("bad chunk json: {e}")))?;
        self.feed_chunk(chunk)
    }

    fn finish_eof(&mut self) -> Result<Vec<StreamEvent>> {
        if self.done {
            return Ok(Vec::new());
        }
        self.finish_now()
    }

    fn is_done(&self) -> bool {
        self.done
    }
}
