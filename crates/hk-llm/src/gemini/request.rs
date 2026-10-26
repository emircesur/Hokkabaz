//! [`ChatRequest`] -> Gemini `streamGenerateContent` JSON body.
//!
//! Wire differences from OpenAI-style APIs handled here: system prompt is the
//! top-level `systemInstruction`, the assistant role is `model`, tool results
//! are `functionResponse` parts of a `user` content (parallel results share
//! one content), calls/results are matched by *name* (there are no call ids on
//! the wire, so ids are synthesised on the way in and mapped back to names
//! here), and tool schemas go through `parametersJsonSchema` so full JSON
//! Schema is accepted without OpenAPI-subset rewriting.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::store::SignatureStore;
use crate::params::retain_supported;
use crate::{ChatRequest, Message, ReasoningEffort, Role, ToolChoice};

/// Google's documented placeholder that disables signature validation for
/// Gemini 3 when the real signature is unavailable (history from another
/// provider or a previous process). Only sent for `gemini-3*` models.
const SKIP_SIGNATURE: &str = "skip_thought_signature_validator";

fn budget_for(e: ReasoningEffort) -> u64 {
    match e {
        ReasoningEffort::Low => 1024,
        ReasoningEffort::Medium => 8192,
        ReasoningEffort::High => 24_576,
    }
}

fn is_v3(model: &str) -> bool {
    model.starts_with("gemini-3")
}

fn has_thinking_knob(model: &str) -> bool {
    is_v3(model) || model.starts_with("gemini-2.5")
}

/// Whether `generationConfig` carries param `key` for `model`.
pub(crate) fn supports(model: &str, key: &str) -> bool {
    match key {
        "temperature" | "top_p" | "top_k" | "max_output_tokens" | "presence_penalty"
        | "frequency_penalty" | "seed" | "stop" | "json_mode" | "json_schema" => true,
        "reasoning_effort" | "thinking_budget" => has_thinking_knob(model),
        // min_p, repetition_penalty, logit_bias: no Gemini equivalent.
        _ => false,
    }
}

/// `thinkingConfig` for the request, or `None` when the model has no
/// thinking knob (Gemini 2.0, Gemma) or the caller did not opt in.
fn thinking_config(req: &ChatRequest) -> Option<Value> {
    if !has_thinking_knob(&req.model) {
        return None;
    }
    // An explicit budget wins; Gemini 3 still accepts `thinkingBudget` but
    // rejects it together with `thinkingLevel`, so it replaces the level.
    if let Some(b) = req.sampling.thinking_budget {
        return Some(json!({ "includeThoughts": b > 0, "thinkingBudget": b }));
    }
    let effort = req.reasoning_effort?;
    if is_v3(&req.model) {
        // Gemini 3 takes a level, not a budget; Medium = model default (Pro has
        // no distinct medium level).
        let mut c = json!({ "includeThoughts": true });
        match effort {
            ReasoningEffort::Low => c["thinkingLevel"] = "low".into(),
            ReasoningEffort::High => c["thinkingLevel"] = "high".into(),
            ReasoningEffort::Medium => {}
        }
        Some(c)
    } else {
        Some(json!({ "includeThoughts": true, "thinkingBudget": budget_for(effort) }))
    }
}

fn tool_config(choice: &ToolChoice) -> Option<Value> {
    let cfg = match choice {
        ToolChoice::Auto => return None,
        ToolChoice::None => json!({ "mode": "NONE" }),
        ToolChoice::Required => json!({ "mode": "ANY" }),
        ToolChoice::Named(n) => json!({ "mode": "ANY", "allowedFunctionNames": [n] }),
    };
    Some(json!({ "functionCallingConfig": cfg }))
}

/// Append `parts` as role `role`, merging into the previous content when the
/// role repeats (parallel tool results, user text after results).
fn push_content(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = contents.last_mut()
        && last["role"] == role
        && let Some(arr) = last["parts"].as_array_mut()
    {
        arr.extend(parts);
        return;
    }
    contents.push(json!({ "role": role, "parts": parts }));
}

fn assistant_parts(m: &Message, model: &str, store: &SignatureStore) -> Vec<Value> {
    let mut parts = Vec::new();
    // Reasoning text is deliberately not replayed: Gemini only wants the
    // opaque signature back, never the thought summary.
    if !m.content.is_empty() {
        parts.push(json!({ "text": m.content }));
    }
    for (i, c) in m.tool_calls.iter().enumerate() {
        let mut p = json!({ "functionCall": { "name": c.name, "args": c.arguments } });
        match store.get(&c.id) {
            Some(sig) => p["thoughtSignature"] = Value::String(sig.to_string()),
            // Only the first call of a step carries a signature on the wire.
            None if i == 0 && is_v3(model) => p["thoughtSignature"] = SKIP_SIGNATURE.into(),
            None => {}
        }
        parts.push(p);
    }
    parts
}

pub(super) fn build_body(req: &ChatRequest, store: &SignatureStore) -> Value {
    let req = &*retain_supported("gemini", req, |k| supports(&req.model, k));
    let mut body = Map::new();

    let system: Vec<&str> = req
        .messages
        .iter()
        .filter(|m| m.role == Role::System && !m.content.is_empty())
        .map(|m| m.content.as_str())
        .collect();
    if !system.is_empty() {
        body.insert(
            "systemInstruction".into(),
            json!({ "parts": [{ "text": system.join("\n\n") }] }),
        );
    }

    // id -> tool name, to label functionResponse parts.
    let names: HashMap<&str, &str> = req
        .messages
        .iter()
        .flat_map(|m| &m.tool_calls)
        .map(|c| (c.id.as_str(), c.name.as_str()))
        .collect();

    let mut contents: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System => {}
            Role::User => {
                // The API rejects empty parts; a blank user turn becomes a space.
                let text = if m.content.is_empty() {
                    " "
                } else {
                    &m.content
                };
                push_content(&mut contents, "user", vec![json!({ "text": text })]);
            }
            Role::Assistant => {
                push_content(
                    &mut contents,
                    "model",
                    assistant_parts(m, &req.model, store),
                );
            }
            Role::Tool => {
                let id = m.tool_call_id.as_deref().unwrap_or_default();
                // Unknown id (truncated history): fall back to the id itself so
                // the request stays well-formed and the model sees something.
                let name = names.get(id).copied().unwrap_or(id);
                push_content(
                    &mut contents,
                    "user",
                    vec![json!({
                        "functionResponse": { "name": name, "response": { "output": m.content } }
                    })],
                );
            }
        }
    }
    body.insert("contents".into(), Value::Array(contents));

    let mut gen_cfg = Map::new();
    if let Some(t) = req.temperature {
        gen_cfg.insert("temperature".into(), json!(t));
    }
    if let Some(n) = req.max_output_tokens {
        gen_cfg.insert("maxOutputTokens".into(), json!(n));
    }
    let p = &req.sampling;
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            gen_cfg.insert(k.into(), v);
        }
    };
    put("topP", p.top_p.map(Value::from));
    put("topK", p.top_k.map(Value::from));
    put("presencePenalty", p.presence_penalty.map(Value::from));
    put("frequencyPenalty", p.frequency_penalty.map(Value::from));
    put("seed", p.seed.map(Value::from));
    put("stopSequences", (!p.stop.is_empty()).then(|| json!(p.stop)));
    if p.json_mode || p.json_schema.is_some() {
        put("responseMimeType", Some(json!("application/json")));
    }
    // `responseJsonSchema` takes full JSON Schema (unlike OpenAPI-subset
    // `responseSchema`), matching `parametersJsonSchema` for tools.
    put("responseJsonSchema", p.json_schema.clone());
    if let Some(t) = thinking_config(req) {
        gen_cfg.insert("thinkingConfig".into(), t);
    }
    if !gen_cfg.is_empty() {
        body.insert("generationConfig".into(), Value::Object(gen_cfg));
    }

    if !req.tools.is_empty() {
        let decls: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parametersJsonSchema": t.parameters,
                })
            })
            .collect();
        body.insert("tools".into(), json!([{ "functionDeclarations": decls }]));
        if let Some(c) = tool_config(&req.tool_choice) {
            body.insert("toolConfig".into(), c);
        }
    }
    Value::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ToolCall, ToolSpec};

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.into(),
            reasoning: Some("hidden thoughts".into()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn history(model: &str) -> ChatRequest {
        let mut asst = msg(Role::Assistant, "");
        asst.tool_calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "ls".into(),
                arguments: json!({"path": "."}),
            },
            ToolCall {
                id: "c2".into(),
                name: "pwd".into(),
                arguments: json!({}),
            },
        ];
        let mut r1 = msg(Role::Tool, "a.rs");
        r1.tool_call_id = Some("c1".into());
        let mut r2 = msg(Role::Tool, "/w");
        r2.tool_call_id = Some("c2".into());
        ChatRequest::new(
            model,
            vec![
                msg(Role::System, "be brief"),
                msg(Role::User, "list"),
                asst,
                r1,
                r2,
            ],
        )
    }

    #[test]
    fn gemini_tool_turn_maps_names_merges_results_and_drops_reasoning() {
        let store = SignatureStore::default();
        store.put("c1", "SIG1");
        let mut r = history("gemini-2.5-pro");
        r.tools = vec![ToolSpec {
            name: "ls".into(),
            description: "d".into(),
            parameters: json!({"type": "object"}),
        }];
        r.tool_choice = ToolChoice::Named("ls".into());
        r.reasoning_effort = Some(ReasoningEffort::Low);
        let b = build_body(&r, &store);
        assert_eq!(b["systemInstruction"]["parts"][0]["text"], "be brief");
        let c = b["contents"].as_array().unwrap();
        assert_eq!(c.len(), 3, "{b}");
        assert_eq!(c[1]["role"], "model");
        assert_eq!(c[1]["parts"][0]["thoughtSignature"], "SIG1");
        assert!(c[1]["parts"][1].get("thoughtSignature").is_none());
        assert_eq!(c[2]["role"], "user");
        assert_eq!(c[2]["parts"][1]["functionResponse"]["name"], "pwd");
        assert_eq!(
            c[2]["parts"][1]["functionResponse"]["response"]["output"],
            "/w"
        );
        assert!(!b.to_string().contains("hidden thoughts"));
        assert_eq!(
            b["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            1024
        );
        assert_eq!(b["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        assert_eq!(
            b["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["type"],
            "object"
        );
    }

    #[test]
    fn gemini3_missing_signature_uses_bypass_on_first_call_only() {
        let b = build_body(&history("gemini-3-pro-preview"), &SignatureStore::default());
        let p = &b["contents"][1]["parts"];
        assert_eq!(p[0]["thoughtSignature"], SKIP_SIGNATURE);
        assert!(p[1].get("thoughtSignature").is_none());
        let b = build_body(&history("gemini-2.5-flash"), &SignatureStore::default());
        assert!(
            b["contents"][1]["parts"][0]
                .get("thoughtSignature")
                .is_none()
        );
    }
}
