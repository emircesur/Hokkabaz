//! Structured output with validation and auto-repair (P9.17): ask for JSON matching a
//! schema, validate it, and on failure feed the errors back for up to `retries` more tries.

use futures::StreamExt;
use serde_json::Value;

use crate::{CancellationToken, ChatRequest, LlmError, Message, Provider, Role, StreamEvent};

/// Outcome of [`complete_structured`].
#[derive(Debug, Clone, PartialEq)]
pub struct Structured {
    pub value: Value,
    /// Tries used (1 = valid on first answer).
    pub attempts: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum StructuredError {
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error("invalid schema: {0}")]
    Schema(String),
    #[error("no valid JSON after {attempts} attempts: {last_error}")]
    Exhausted { attempts: u32, last_error: String },
}

/// First JSON object/array in `text` (tolerates prose and ``` fences around it).
pub fn extract_json(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str(t) {
        return Some(v);
    }
    let start = t.find(['{', '['])?;
    let (open, close) = if t[start..].starts_with('{') {
        ('{', '}')
    } else {
        ('[', ']')
    };
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    for (i, c) in t[start..].char_indices() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            c if c == open => depth += 1,
            c if c == close => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&t[start..=start + i]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

/// Validation errors of `v` against a compiled schema, joined; `None` = valid.
pub fn schema_errors(schema: &jsonschema::Validator, v: &Value) -> Option<String> {
    let errs: Vec<String> = schema
        .iter_errors(v)
        .take(8)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    (!errs.is_empty()).then(|| errs.join("; "))
}

fn msg(role: Role, content: String) -> Message {
    Message { images: Vec::new(),
        role,
        content,
        reasoning: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}

async fn collect_text(
    provider: &dyn Provider,
    req: ChatRequest,
    cancel: &CancellationToken,
) -> Result<String, LlmError> {
    let mut s = provider.stream(req, cancel.clone()).await?;
    let mut text = String::new();
    while let Some(ev) = s.next().await {
        match ev? {
            StreamEvent::TextDelta { text: t } => text.push_str(&t),
            StreamEvent::Finish(_) => break,
            _ => {}
        }
    }
    Ok(text)
}

/// Run `req` constrained to `schema` (sent as `json_schema` where the provider supports it,
/// always stated in the prompt), validating and repairing up to `retries` extra times.
pub async fn complete_structured(
    provider: &dyn Provider,
    mut req: ChatRequest,
    schema: &Value,
    retries: u32,
    cancel: &CancellationToken,
) -> Result<Structured, StructuredError> {
    let validator =
        jsonschema::validator_for(schema).map_err(|e| StructuredError::Schema(e.to_string()))?;
    req.sampling.json_schema = Some(schema.clone());
    req.messages.push(msg(
        Role::System,
        format!("Reply with one JSON value only, matching this JSON Schema:\n{schema}"),
    ));
    let mut last_error = String::new();
    for attempt in 1..=retries + 1 {
        let text = collect_text(provider, req.clone(), cancel).await?;
        let problem = match extract_json(&text) {
            None => "the reply contained no parseable JSON".to_string(),
            Some(v) => match schema_errors(&validator, &v) {
                None => {
                    return Ok(Structured {
                        value: v,
                        attempts: attempt,
                    });
                }
                Some(e) => e,
            },
        };
        last_error = problem.clone();
        req.messages.push(msg(Role::Assistant, text));
        req.messages.push(msg(
            Role::User,
            format!("That JSON is invalid: {problem}. Reply again with corrected JSON only."),
        ));
    }
    Err(StructuredError::Exhausted {
        attempts: retries + 1,
        last_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_from_prose_and_fences() {
        assert_eq!(
            extract_json("Sure:\n```json\n{\"a\": \"}\"}\n```"),
            Some(json!({"a": "}"}))
        );
        assert_eq!(extract_json("[1, 2] trailing"), Some(json!([1, 2])));
        assert_eq!(extract_json("no json"), None);
    }

    #[test]
    fn validates() {
        let v = jsonschema::validator_for(
            &json!({"type":"object","required":["n"],"properties":{"n":{"type":"integer"}}}),
        )
        .unwrap();
        assert!(schema_errors(&v, &json!({"n": 1})).is_none());
        assert!(schema_errors(&v, &json!({"n": "x"})).is_some());
    }
}
