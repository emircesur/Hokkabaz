//! Minimal LLM seam for Studio prompts: one system + one user message in, text
//! (or parsed JSON) out. Real models come from `hk-llm`; tests use [`ScriptedLlm`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use hk_llm::{CancellationToken, ChatRequest, Message, Provider, Role, StreamEvent};
use serde::de::DeserializeOwned;

use crate::error::{Result, StudioError};

#[async_trait]
pub trait Llm: Send + Sync {
    async fn complete(&self, system: &str, user: &str) -> Result<String>;
}

/// [`Llm`] over any `hk_llm::Provider` (own API keys, Ollama, ...).
#[derive(Clone)]
pub struct ProviderLlm {
    provider: Arc<dyn Provider>,
    model: String,
    temperature: Option<f32>,
    max_output_tokens: Option<u64>,
}

impl std::fmt::Debug for ProviderLlm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderLlm")
            .field("provider", &self.provider.id())
            .field("model", &self.model)
            .finish()
    }
}

impl ProviderLlm {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            temperature: Some(0.3),
            max_output_tokens: Some(4096),
        }
    }

    pub fn with_temperature(mut self, t: Option<f32>) -> Self {
        self.temperature = t;
        self
    }

    pub fn with_max_output_tokens(mut self, n: Option<u64>) -> Self {
        self.max_output_tokens = n;
        self
    }
}

fn msg(role: Role, content: &str) -> Message {
    Message {
        role,
        content: content.to_string(),
        reasoning: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}

#[async_trait]
impl Llm for ProviderLlm {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let mut req = ChatRequest::new(
            self.model.clone(),
            vec![msg(Role::System, system), msg(Role::User, user)],
        );
        req.temperature = self.temperature;
        req.max_output_tokens = self.max_output_tokens;
        let mut stream = self
            .provider
            .stream(req, CancellationToken::new())
            .await
            .map_err(|e| StudioError::Llm(e.to_string()))?;
        let mut text = String::new();
        while let Some(ev) = stream.next().await {
            match ev.map_err(|e| StudioError::Llm(e.to_string()))? {
                StreamEvent::TextDelta { text: t } => text.push_str(&t),
                StreamEvent::Finish(_) => break,
                _ => {}
            }
        }
        Ok(text)
    }
}

/// Pops canned replies in order; records prompts. For tests.
#[derive(Debug, Default)]
pub struct ScriptedLlm {
    replies: Mutex<VecDeque<String>>,
    pub prompts: Mutex<Vec<(String, String)>>,
}

impl ScriptedLlm {
    pub fn new(replies: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().map(Into::into).collect()),
            prompts: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Llm for ScriptedLlm {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        self.prompts
            .lock()
            .expect("lock")
            .push((system.to_string(), user.to_string()));
        self.replies
            .lock()
            .expect("lock")
            .pop_front()
            .ok_or_else(|| StudioError::Llm("scripted llm out of replies".into()))
    }
}

/// First balanced JSON object/array in `text` (models wrap JSON in prose or fences).
pub fn extract_json(text: &str) -> Result<serde_json::Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str(t) {
        return Ok(v);
    }
    let bytes: Vec<char> = t.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let open = bytes[i];
        if open == '{' || open == '[' {
            let close = if open == '{' { '}' } else { ']' };
            let (mut depth, mut in_str, mut esc) = (0i32, false, false);
            for (j, &c) in bytes.iter().enumerate().skip(i) {
                if in_str {
                    if esc {
                        esc = false;
                    } else if c == '\\' {
                        esc = true;
                    } else if c == '"' {
                        in_str = false;
                    }
                    continue;
                }
                if c == '"' {
                    in_str = true;
                } else if c == open {
                    depth += 1;
                } else if c == close {
                    depth -= 1;
                    if depth == 0 {
                        let s: String = bytes[i..=j].iter().collect();
                        if let Ok(v) = serde_json::from_str(&s) {
                            return Ok(v);
                        }
                        break;
                    }
                }
            }
        }
        i += 1;
    }
    Err(StudioError::Llm(format!(
        "no JSON in model output: {}",
        t.chars().take(120).collect::<String>()
    )))
}

/// Ask for JSON and parse it into `T`; one repair retry that feeds the error back.
pub async fn complete_json<T: DeserializeOwned>(
    llm: &dyn Llm,
    system: &str,
    user: &str,
) -> Result<T> {
    let first = llm.complete(system, user).await?;
    match extract_json(&first)
        .and_then(|v| serde_json::from_value::<T>(v).map_err(|e| StudioError::Llm(e.to_string())))
    {
        Ok(v) => Ok(v),
        Err(err) => {
            let retry = format!(
                "{user}\n\nYour previous answer was not valid ({err}). Reply with ONLY the corrected JSON, no prose."
            );
            let second = llm.complete(system, &retry).await?;
            let v = extract_json(&second)?;
            serde_json::from_value(v).map_err(|e| StudioError::Llm(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Out {
        n: u32,
    }

    #[test]
    fn extracts_from_fences_and_prose() {
        assert_eq!(extract_json("```json\n{\"n\": 1}\n```").unwrap()["n"], 1);
        assert_eq!(
            extract_json("Sure! Here: {\"a\": \"}\"} thanks").unwrap()["a"],
            "}"
        );
        assert_eq!(extract_json("[1,2]").unwrap()[1], 2);
        assert!(extract_json("nothing").is_err());
    }

    #[tokio::test]
    async fn complete_json_retries_once() {
        let llm = ScriptedLlm::new(["not json at all", "{\"n\": 7}"]);
        let out: Out = complete_json(&llm, "s", "u").await.unwrap();
        assert_eq!(out, Out { n: 7 });
        assert_eq!(llm.prompts.lock().unwrap().len(), 2);
        assert!(llm.prompts.lock().unwrap()[1].1.contains("corrected JSON"));
        let bad = ScriptedLlm::new(["x", "y"]);
        assert!(complete_json::<Out>(&bad, "s", "u").await.is_err());
    }
}
