//! Scripted chat models for tests and offline demos: no network, deterministic.
//!
//! A script maps a model name to a queue of replies, consumed in call order across every
//! node that builds that model:
//!
//! ```json
//! { "models": { "gpt-test": [
//!     { "tool_calls": [{ "name": "lookup", "arguments": { "input": "77" } }] },
//!     { "text": "{\"city\": \"Rome\"}", "expect_last_contains": "shipped" }
//! ] } }
//! ```
//!
//! `expect_last_contains` asserts the last message the model was shown (the tool
//! observation in a tool loop) contains the text; a miss fails the run.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use hk_llm::{FinishReason, Message, ToolCall, ToolSpec, Usage};
use serde_json::Value;

use super::types::{ChatModel, ChatModelFactory, ChatModelSpec, Completion, ai_err};
use crate::error::FlowError;

#[derive(Debug, Clone)]
struct Step {
    reply: Completion,
    expect_last_contains: Option<String>,
}

/// What a scripted model was asked.
#[derive(Debug, Clone)]
pub struct SeenRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Default)]
struct Shared {
    queues: BTreeMap<String, VecDeque<Step>>,
    seen: Vec<SeenRequest>,
}

/// Factory + model in one: every `build` returns a handle onto the shared script.
#[derive(Debug, Clone, Default)]
pub struct ScriptedChatModelFactory(Arc<Mutex<Shared>>);

impl ScriptedChatModelFactory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Script from the JSON shape in the module docs.
    pub fn from_json(v: &Value) -> Result<Self, FlowError> {
        let f = Self::new();
        let models = v
            .get("models")
            .and_then(Value::as_object)
            .ok_or_else(|| ai_err("script needs a `models` object"))?;
        for (name, steps) in models {
            for s in steps.as_array().map(Vec::as_slice).unwrap_or(&[]) {
                let calls = s
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .enumerate()
                            .map(|(i, c)| ToolCall {
                                id: c
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| format!("call_{i}")),
                                name: c
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                arguments: c.get("arguments").cloned().unwrap_or(Value::Null),
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                f.push_step(
                    name,
                    Step {
                        reply: Completion {
                            text: s
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            finish: if calls.is_empty() {
                                FinishReason::Stop
                            } else {
                                FinishReason::ToolCalls
                            },
                            tool_calls: calls,
                            usage: Usage::default(),
                        },
                        expect_last_contains: s
                            .get("expect_last_contains")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    },
                );
            }
        }
        Ok(f)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push_step(&self, model: &str, step: Step) {
        self.lock()
            .queues
            .entry(model.to_owned())
            .or_default()
            .push_back(step);
    }

    /// Queue a plain text reply for `model`.
    pub fn push_text(&self, model: &str, text: &str) {
        self.push_step(
            model,
            Step {
                reply: Completion {
                    text: text.to_owned(),
                    tool_calls: Vec::new(),
                    finish: FinishReason::Stop,
                    usage: Usage::default(),
                },
                expect_last_contains: None,
            },
        );
    }

    /// Every request served so far, oldest first.
    pub fn seen(&self) -> Vec<SeenRequest> {
        self.lock().seen.clone()
    }
}

#[derive(Debug)]
struct ScriptedModel {
    name: String,
    script: ScriptedChatModelFactory,
}

#[async_trait]
impl ChatModel for ScriptedModel {
    fn model_name(&self) -> &str {
        &self.name
    }

    async fn complete(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolSpec>,
    ) -> Result<Completion, FlowError> {
        let mut s = self.script.lock();
        let last = messages
            .last()
            .map(|m| m.content.clone())
            .unwrap_or_default();
        s.seen.push(SeenRequest {
            model: self.name.clone(),
            messages,
            tools,
        });
        let step = s
            .queues
            .get_mut(&self.name)
            .and_then(VecDeque::pop_front)
            .ok_or_else(|| ai_err(format!("scripted model `{}` has no reply left", self.name)))?;
        if let Some(want) = &step.expect_last_contains
            && !last.contains(want.as_str())
        {
            return Err(ai_err(format!(
                "scripted model `{}` expected the last message to contain `{want}`, got `{last}`",
                self.name
            )));
        }
        Ok(step.reply)
    }
}

#[async_trait]
impl ChatModelFactory for ScriptedChatModelFactory {
    async fn build(&self, spec: ChatModelSpec) -> Result<Arc<dyn ChatModel>, FlowError> {
        Ok(Arc::new(ScriptedModel {
            name: spec.model,
            script: self.clone(),
        }))
    }
}
