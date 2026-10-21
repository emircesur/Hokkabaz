//! SSE payloads -> [`StreamEvent`]s for OpenAI-style chat chunks.
//!
//! Superset of the dialects seen so far: DeepSeek cache counters, OpenAI
//! `cached_tokens`, `reasoning_content` / `reasoning` deltas.

use std::collections::VecDeque;
use std::fmt::Display;
use std::time::Duration;

use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::error::is_context_overflow;
use super::sse::SseDecoder;
use crate::{EventStream, FinishReason, LlmError, Result, StreamEvent, Usage};

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Option<WireUsage>,
    error: Option<WireError>,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Delta {
    content: Option<String>,
    // `reasoning_content` (DeepSeek, vLLM, Qwen) or `reasoning` (OpenRouter, Ollama).
    #[serde(alias = "reasoning")]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolDelta>,
}

#[derive(Deserialize)]
struct ToolDelta {
    #[serde(default)]
    index: u32,
    id: Option<String>,
    function: Option<FnDelta>,
}

#[derive(Deserialize, Default)]
struct FnDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    prompt_cache_hit_tokens: Option<u64>,
    prompt_cache_miss_tokens: Option<u64>,
    // OpenAI-style cache counter.
    prompt_tokens_details: Option<PromptDetails>,
    completion_tokens_details: Option<CompletionDetails>,
}

#[derive(Deserialize)]
struct PromptDetails {
    #[serde(default)]
    cached_tokens: u64,
}

#[derive(Deserialize)]
struct CompletionDetails {
    #[serde(default)]
    reasoning_tokens: u64,
}

impl WireUsage {
    fn into_usage(self) -> Usage {
        let hit = self
            .prompt_cache_hit_tokens
            .or_else(|| self.prompt_tokens_details.map(|d| d.cached_tokens))
            .unwrap_or(0);
        // Prefer the explicit miss counter; otherwise derive it.
        let miss = self
            .prompt_cache_miss_tokens
            .unwrap_or_else(|| self.prompt_tokens.saturating_sub(hit));
        Usage {
            input_tokens: miss,
            output_tokens: self.completion_tokens,
            cache_read_tokens: hit,
            cache_write_tokens: 0,
            reasoning_tokens: self
                .completion_tokens_details
                .map_or(0, |d| d.reasoning_tokens),
        }
    }
}

fn finish_reason(s: &str) -> FinishReason {
    match s {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        _ => FinishReason::Other,
    }
}

/// Stateful chunk parser. `Finish` is held back until `[DONE]` (or EOF) so it
/// is always the last event, after the trailing usage chunk.
#[derive(Debug, Default)]
pub struct ChunkParser {
    finish: Option<FinishReason>,
    done: bool,
}

impl ChunkParser {
    /// True once `Finish` has been emitted.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Parse one SSE `data` payload.
    pub fn feed(&mut self, data: &str) -> Result<Vec<StreamEvent>> {
        if self.done {
            return Ok(Vec::new());
        }
        let data = data.trim();
        if data == "[DONE]" {
            return self.finish_now();
        }
        let chunk: Chunk = serde_json::from_str(data)
            .map_err(|e| LlmError::Protocol(format!("bad chunk json: {e}")))?;
        if let Some(e) = chunk.error {
            return Err(if is_context_overflow(&e.message) {
                LlmError::ContextLength(e.message)
            } else {
                LlmError::Protocol(format!("provider error mid-stream: {}", e.message))
            });
        }
        let mut out = Vec::new();
        // n=1 always; extra choices are ignored.
        if let Some(choice) = chunk.choices.into_iter().next() {
            let d = choice.delta;
            if let Some(t) = d.reasoning_content.filter(|t| !t.is_empty()) {
                out.push(StreamEvent::ReasoningDelta { text: t });
            }
            if let Some(t) = d.content.filter(|t| !t.is_empty()) {
                out.push(StreamEvent::TextDelta { text: t });
            }
            for tc in d.tool_calls {
                let f = tc.function.unwrap_or_default();
                out.push(StreamEvent::ToolCallDelta {
                    index: tc.index,
                    id: tc.id.filter(|s| !s.is_empty()),
                    name: f.name.filter(|s| !s.is_empty()),
                    arguments_delta: f.arguments.unwrap_or_default(),
                });
            }
            if let Some(r) = choice.finish_reason {
                self.finish = Some(finish_reason(&r));
            }
        }
        if let Some(u) = chunk.usage {
            out.push(StreamEvent::Usage(u.into_usage()));
        }
        Ok(out)
    }

    /// Body ended. Tolerates a missing `[DONE]` if a finish reason was seen.
    pub fn finish_eof(&mut self) -> Result<Vec<StreamEvent>> {
        if self.done {
            return Ok(Vec::new());
        }
        self.finish_now()
    }

    fn finish_now(&mut self) -> Result<Vec<StreamEvent>> {
        let reason = self
            .finish
            .ok_or_else(|| LlmError::Protocol("stream ended without finish_reason".into()))?;
        self.done = true;
        Ok(vec![StreamEvent::Finish(reason)])
    }
}

/// Incremental payload parser driven by [`decode_stream_with`]. Implemented by
/// the OpenAI chunk parser and by other SSE dialects (Anthropic) so they share
/// the idle-timeout/cancel/EOF handling below.
pub trait PayloadParser: Send + 'static {
    /// Parse one SSE `data` payload.
    fn feed(&mut self, data: &str) -> Result<Vec<StreamEvent>>;
    /// Body ended without the terminal marker.
    fn finish_eof(&mut self) -> Result<Vec<StreamEvent>>;
    /// True once the terminal event has been emitted.
    fn is_done(&self) -> bool;
}

impl PayloadParser for ChunkParser {
    fn feed(&mut self, data: &str) -> Result<Vec<StreamEvent>> {
        ChunkParser::feed(self, data)
    }
    fn finish_eof(&mut self) -> Result<Vec<StreamEvent>> {
        ChunkParser::finish_eof(self)
    }
    fn is_done(&self) -> bool {
        ChunkParser::is_done(self)
    }
}

struct State<S, P> {
    src: S,
    sse: SseDecoder,
    parser: P,
    queue: VecDeque<StreamEvent>,
    pending_err: Option<LlmError>,
    cancel: CancellationToken,
    idle: Duration,
    ended: bool,
}

impl<S, P: PayloadParser> State<S, P> {
    fn feed_all(&mut self, payloads: Vec<String>) {
        for p in payloads {
            match self.parser.feed(&p) {
                Ok(evs) => self.queue.extend(evs),
                Err(e) => {
                    self.pending_err = Some(e);
                    self.ended = true;
                    return;
                }
            }
            if self.parser.is_done() {
                self.ended = true;
                return;
            }
        }
    }
}

/// Turn an HTTP body byte stream into normalised events.
///
/// Errors are yielded in-band as the last item. `idle` bounds the wait for
/// the next network chunk (DeepSeek sends `: keep-alive` comments while busy).
pub fn decode_stream<S, E>(body: S, cancel: CancellationToken, idle: Duration) -> EventStream
where
    S: Stream<Item = std::result::Result<Bytes, E>> + Send + Unpin + 'static,
    E: Display + Send + 'static,
{
    decode_stream_with(body, ChunkParser::default(), cancel, idle)
}

/// [`decode_stream`] with a caller-supplied dialect parser.
pub fn decode_stream_with<S, E, P>(
    body: S,
    parser: P,
    cancel: CancellationToken,
    idle: Duration,
) -> EventStream
where
    S: Stream<Item = std::result::Result<Bytes, E>> + Send + Unpin + 'static,
    E: Display + Send + 'static,
    P: PayloadParser,
{
    let st = State {
        src: body,
        sse: SseDecoder::default(),
        parser,
        queue: VecDeque::new(),
        pending_err: None,
        cancel,
        idle,
        ended: false,
    };
    stream::unfold(st, |mut st| async move {
        loop {
            if let Some(ev) = st.queue.pop_front() {
                return Some((Ok(ev), st));
            }
            if let Some(e) = st.pending_err.take() {
                return Some((Err(e), st));
            }
            if st.ended {
                return None;
            }
            let next = tokio::select! {
                biased;
                () = st.cancel.cancelled() => {
                    st.ended = true;
                    st.pending_err = Some(LlmError::Cancelled);
                    continue;
                }
                r = tokio::time::timeout(st.idle, st.src.next()) => r,
            };
            match next {
                Err(_) => {
                    st.ended = true;
                    st.pending_err = Some(LlmError::Network("stream idle timeout".into()));
                }
                Ok(Some(Ok(bytes))) => {
                    let payloads = st.sse.push(&bytes);
                    st.feed_all(payloads);
                }
                Ok(Some(Err(e))) => {
                    st.ended = true;
                    st.pending_err = Some(LlmError::Network(e.to_string()));
                }
                Ok(None) => {
                    let payloads = st.sse.finish();
                    st.feed_all(payloads);
                    if !st.ended {
                        st.ended = true;
                        match st.parser.finish_eof() {
                            Ok(evs) => st.queue.extend(evs),
                            Err(e) => st.pending_err = Some(e),
                        }
                    }
                }
            }
        }
    })
    .boxed()
}
