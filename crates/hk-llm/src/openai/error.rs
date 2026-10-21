//! HTTP error mapping for OpenAI-style APIs.
//!
//! Shared by every `/chat/completions` backend. Codes: 400/422 bad request,
//! 401/403 auth, 402 balance (DeepSeek), 429 rate limit, 5xx server busy.

use std::time::Duration;

use serde_json::Value;

use crate::LlmError;

/// Map a non-2xx response to an [`LlmError`]. `body` is the raw response text.
pub fn error_from_response(status: u16, retry_after: Option<Duration>, body: &str) -> LlmError {
    let message = extract_message(body).unwrap_or_else(|| truncate(body.trim(), 500));
    match status {
        401 | 403 => LlmError::Auth(message),
        400 | 422 if is_context_overflow(&message) => LlmError::ContextLength(message),
        400 | 422 => LlmError::InvalidRequest(message),
        _ => LlmError::Http {
            status,
            message,
            retry_after,
        },
    }
}

/// `{"error":{"message":..}}` -> message. Also accepts `{"error":".."}` and
/// the flat `{"message":..}` / `{"detail":..}` shapes some gateways return.
pub(super) fn extract_message(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    if let Some(e) = v.get("error") {
        return e
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| e.as_str())
            .map(str::to_owned);
    }
    v.get("message")
        .or_else(|| v.get("detail"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub(super) fn is_context_overflow(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("context length")
        || m.contains("maximum context")
        || m.contains("context_length")
        // Anthropic: "prompt is too long: N tokens > M maximum"
        || m.contains("prompt is too long")
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}...", &s[..i]),
        None => s.to_owned(),
    }
}
