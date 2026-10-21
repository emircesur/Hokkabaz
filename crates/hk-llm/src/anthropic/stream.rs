//! Anthropic SSE events -> [`StreamEvent`]s.
//!
//! Event order: `message_start`, then per block `content_block_start`,
//! `content_block_delta`*, `content_block_stop`, then `message_delta`
//! (stop reason + cumulative usage) and `message_stop`. `ping` is a
//! keep-alive; unknown event types are ignored for forward compatibility.

use serde::Deserialize;
use std::collections::HashMap;

use super::store::{ReasoningStore, ThinkingBlock};
use crate::openai::PayloadParser;
use crate::{FinishReason, LlmError, Result, StreamEvent, Usage};

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    MessageStart {
        message: StartMessage,
    },
    ContentBlockStart {
        index: u64,
        content_block: BlockStart,
    },
    ContentBlockDelta {
        index: u64,
        delta: Delta,
    },
    ContentBlockStop {
        index: u64,
    },
    MessageDelta {
        delta: MessageDelta,
        usage: Option<WireUsage>,
    },
    MessageStop,
    Ping,
    Error {
        error: WireError,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct StartMessage {
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BlockStart {
    Text {
        #[serde(default)]
        text: String,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: String,
    },
    RedactedThinking {
        #[serde(default)]
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Delta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "signature_delta")]
    Signature { signature: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct MessageDelta {
    stop_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    message: String,
}

enum Block {
    Text,
    Thinking { text: String, signature: String },
    Redacted(String),
    Tool { index: u32, had_args: bool },
    Other,
}

/// Stateful per-response parser. `Finish` is held until `message_stop`.
pub(crate) struct AnthropicParser {
    store: ReasoningStore,
    blocks: HashMap<u64, Block>,
    tool_count: u32,
    tool_ids: Vec<String>,
    thinking: Vec<ThinkingBlock>,
    usage: Usage,
    finish: Option<FinishReason>,
    done: bool,
}

impl std::fmt::Debug for AnthropicParser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicParser")
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

fn stop_reason(s: &str) -> FinishReason {
    match s {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "refusal" => FinishReason::ContentFilter,
        _ => FinishReason::Other,
    }
}

fn error_event(e: WireError) -> LlmError {
    let m = e.message;
    match e.kind.as_str() {
        // Mapped to HTTP-equivalent statuses so the router's retry/fallback
        // logic treats in-stream errors like pre-stream ones.
        "overloaded_error" => LlmError::Http {
            status: 529,
            message: m,
            retry_after: None,
        },
        "rate_limit_error" => LlmError::Http {
            status: 429,
            message: m,
            retry_after: None,
        },
        "api_error" => LlmError::Http {
            status: 500,
            message: m,
            retry_after: None,
        },
        "authentication_error" | "permission_error" => LlmError::Auth(m),
        "invalid_request_error" if m.to_ascii_lowercase().contains("prompt is too long") => {
            LlmError::ContextLength(m)
        }
        "invalid_request_error" => LlmError::InvalidRequest(m),
        _ => LlmError::Protocol(format!("provider error mid-stream: {} {m}", e.kind)),
    }
}

impl AnthropicParser {
    pub(crate) fn new(store: ReasoningStore) -> Self {
        Self {
            store,
            blocks: HashMap::new(),
            tool_count: 0,
            tool_ids: Vec::new(),
            thinking: Vec::new(),
            usage: Usage::default(),
            finish: None,
            done: false,
        }
    }

    fn merge_usage(&mut self, u: WireUsage) -> StreamEvent {
        // Later events carry cumulative values; only fields present override.
        if let Some(v) = u.input_tokens {
            self.usage.input_tokens = v;
        }
        if let Some(v) = u.output_tokens {
            self.usage.output_tokens = v;
        }
        if let Some(v) = u.cache_read_input_tokens {
            self.usage.cache_read_tokens = v;
        }
        if let Some(v) = u.cache_creation_input_tokens {
            self.usage.cache_write_tokens = v;
        }
        StreamEvent::Usage(self.usage)
    }

    fn event(&mut self, ev: Event) -> Result<Vec<StreamEvent>> {
        let mut out = Vec::new();
        match ev {
            Event::MessageStart { message } => {
                if let Some(u) = message.usage {
                    out.push(self.merge_usage(u));
                }
            }
            Event::ContentBlockStart {
                index,
                content_block,
            } => match content_block {
                BlockStart::Text { text } => {
                    self.blocks.insert(index, Block::Text);
                    if !text.is_empty() {
                        out.push(StreamEvent::TextDelta { text });
                    }
                }
                BlockStart::Thinking {
                    thinking,
                    signature,
                } => {
                    if !thinking.is_empty() {
                        out.push(StreamEvent::ReasoningDelta {
                            text: thinking.clone(),
                        });
                    }
                    self.blocks.insert(
                        index,
                        Block::Thinking {
                            text: thinking,
                            signature,
                        },
                    );
                }
                BlockStart::RedactedThinking { data } => {
                    self.blocks.insert(index, Block::Redacted(data));
                }
                BlockStart::ToolUse { id, name } => {
                    let n = self.tool_count;
                    self.tool_count += 1;
                    self.tool_ids.push(id.clone());
                    self.blocks.insert(
                        index,
                        Block::Tool {
                            index: n,
                            had_args: false,
                        },
                    );
                    out.push(StreamEvent::ToolCallDelta {
                        index: n,
                        id: Some(id),
                        name: Some(name),
                        arguments_delta: String::new(),
                    });
                }
                BlockStart::Other => {
                    self.blocks.insert(index, Block::Other);
                }
            },
            Event::ContentBlockDelta { index, delta } => match (self.blocks.get_mut(&index), delta)
            {
                (Some(Block::Text), Delta::Text { text }) if !text.is_empty() => {
                    out.push(StreamEvent::TextDelta { text });
                }
                (Some(Block::Thinking { text: acc, .. }), Delta::Thinking { thinking }) => {
                    if !thinking.is_empty() {
                        acc.push_str(&thinking);
                        out.push(StreamEvent::ReasoningDelta { text: thinking });
                    }
                }
                (Some(Block::Thinking { signature, .. }), Delta::Signature { signature: s }) => {
                    signature.push_str(&s);
                }
                (Some(Block::Tool { index, had_args }), Delta::InputJson { partial_json })
                    if !partial_json.is_empty() =>
                {
                    *had_args = true;
                    out.push(StreamEvent::ToolCallDelta {
                        index: *index,
                        id: None,
                        name: None,
                        arguments_delta: partial_json,
                    });
                }
                _ => {}
            },
            Event::ContentBlockStop { index } => match self.blocks.remove(&index) {
                Some(Block::Thinking { text, signature }) => {
                    // Without a signature (display "omitted" still signs) the
                    // block is not replayable; drop it.
                    if !signature.is_empty() {
                        self.thinking.push(ThinkingBlock::Thinking {
                            thinking: text,
                            signature,
                        });
                    }
                }
                Some(Block::Redacted(data)) => {
                    self.thinking.push(ThinkingBlock::Redacted { data });
                }
                Some(Block::Tool {
                    index,
                    had_args: false,
                }) => {
                    // Tool without parameters streams no input_json_delta;
                    // the agent expects a JSON object.
                    out.push(StreamEvent::ToolCallDelta {
                        index,
                        id: None,
                        name: None,
                        arguments_delta: "{}".into(),
                    });
                }
                _ => {}
            },
            Event::MessageDelta { delta, usage } => {
                if let Some(r) = delta.stop_reason {
                    self.finish = Some(stop_reason(&r));
                }
                if let Some(u) = usage {
                    out.push(self.merge_usage(u));
                }
            }
            Event::MessageStop => return self.finish_now(),
            Event::Error { error } => return Err(error_event(error)),
            Event::Ping | Event::Unknown => {}
        }
        Ok(out)
    }

    fn finish_now(&mut self) -> Result<Vec<StreamEvent>> {
        let reason = self
            .finish
            .ok_or_else(|| LlmError::Protocol("stream ended without stop_reason".into()))?;
        self.done = true;
        self.store
            .put(&self.tool_ids, std::mem::take(&mut self.thinking));
        Ok(vec![StreamEvent::Finish(reason)])
    }
}

impl PayloadParser for AnthropicParser {
    fn feed(&mut self, data: &str) -> Result<Vec<StreamEvent>> {
        if self.done {
            return Ok(Vec::new());
        }
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let ev: Event = serde_json::from_str(data)
            .map_err(|e| LlmError::Protocol(format!("bad event json: {e}")))?;
        self.event(ev)
    }

    /// Tolerates a missing `message_stop` if a stop reason was seen.
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
