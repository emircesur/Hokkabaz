//! Ollama NDJSON lines -> normalised events (`/api/chat`) and pull progress
//! (`/api/pull`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ndjson::LineParser;
use crate::{FinishReason, LlmError, Result, StreamEvent, Usage};

#[derive(Deserialize)]
struct ChatLine {
    message: Option<WireMessage>,
    #[serde(default)]
    done: bool,
    done_reason: Option<String>,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
    error: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireMessage {
    content: Option<String>,
    thinking: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireToolCall {
    id: Option<String>,
    function: WireFn,
}

#[derive(Deserialize)]
struct WireFn {
    name: String,
    #[serde(default)]
    arguments: Value,
}

/// Chat line parser. Tool calls arrive whole (no incremental arguments), so
/// each becomes a single [`StreamEvent::ToolCallDelta`] with a synthesized id.
#[derive(Debug, Default)]
pub struct ChatParser {
    tool_calls: u32,
    done: bool,
}

impl LineParser for ChatParser {
    type Out = StreamEvent;

    fn feed(&mut self, line: &str) -> Result<Vec<StreamEvent>> {
        if self.done {
            return Ok(Vec::new());
        }
        let l: ChatLine = serde_json::from_str(line)
            .map_err(|e| LlmError::Protocol(format!("bad chat line json: {e}")))?;
        if let Some(e) = l.error {
            return Err(LlmError::Protocol(format!(
                "provider error mid-stream: {e}"
            )));
        }
        let mut out = Vec::new();
        if let Some(m) = l.message {
            if let Some(t) = m.thinking.filter(|t| !t.is_empty()) {
                out.push(StreamEvent::ReasoningDelta { text: t });
            }
            if let Some(t) = m.content.filter(|t| !t.is_empty()) {
                out.push(StreamEvent::TextDelta { text: t });
            }
            for tc in m.tool_calls {
                let index = self.tool_calls;
                self.tool_calls += 1;
                let args = match tc.function.arguments {
                    Value::Null => "{}".to_owned(),
                    // Some builds send the arguments pre-encoded.
                    Value::String(s) => s,
                    v => v.to_string(),
                };
                out.push(StreamEvent::ToolCallDelta {
                    index,
                    id: Some(
                        tc.id
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| format!("ollama_call_{index}")),
                    ),
                    name: Some(tc.function.name),
                    arguments_delta: args,
                });
            }
        }
        if l.done {
            self.done = true;
            if l.prompt_eval_count.is_some() || l.eval_count.is_some() {
                out.push(StreamEvent::Usage(Usage {
                    input_tokens: l.prompt_eval_count.unwrap_or(0),
                    output_tokens: l.eval_count.unwrap_or(0),
                    ..Usage::default()
                }));
            }
            out.push(StreamEvent::Finish(finish_reason(
                l.done_reason.as_deref(),
                self.tool_calls > 0,
            )));
        }
        Ok(out)
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn finish_eof(&mut self) -> Result<Vec<StreamEvent>> {
        Err(LlmError::Protocol("stream ended without done".into()))
    }
}

fn finish_reason(reason: Option<&str>, had_tool_calls: bool) -> FinishReason {
    // Ollama reports `stop` even when the turn ended in tool calls.
    match reason {
        Some("stop") | None if had_tool_calls => FinishReason::ToolCalls,
        Some("stop") | None => FinishReason::Stop,
        Some("length") => FinishReason::Length,
        Some(_) => FinishReason::Other,
    }
}

/// One progress update from `/api/pull`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullProgress {
    /// Phase text, e.g. `pulling manifest`, `pulling <digest>`, `success`.
    pub status: String,
    /// Layer digest while downloading a layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Layer size in bytes, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Bytes of the layer downloaded so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<u64>,
}

impl PullProgress {
    /// Terminal update: the model is installed.
    pub fn is_success(&self) -> bool {
        self.status == "success"
    }

    /// Download fraction of the current layer in `0.0..=1.0`, if known.
    #[allow(clippy::cast_precision_loss)] // progress display only
    pub fn fraction(&self) -> Option<f64> {
        match (self.completed, self.total) {
            (Some(c), Some(t)) if t > 0 => Some((c as f64 / t as f64).min(1.0)),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct PullLine {
    error: Option<String>,
    #[serde(flatten)]
    progress: Option<PullProgress>,
}

#[derive(Debug, Default)]
pub(super) struct PullParser {
    done: bool,
}

impl LineParser for PullParser {
    type Out = PullProgress;

    fn feed(&mut self, line: &str) -> Result<Vec<PullProgress>> {
        if self.done {
            return Ok(Vec::new());
        }
        let l: PullLine = serde_json::from_str(line)
            .map_err(|e| LlmError::Protocol(format!("bad pull line json: {e}")))?;
        if let Some(e) = l.error {
            return Err(LlmError::Protocol(format!("pull failed: {e}")));
        }
        let Some(p) = l.progress else {
            return Ok(Vec::new());
        };
        self.done = p.is_success();
        Ok(vec![p])
    }

    fn is_done(&self) -> bool {
        self.done
    }

    fn finish_eof(&mut self) -> Result<Vec<PullProgress>> {
        Err(LlmError::Protocol("pull ended without success".into()))
    }
}
