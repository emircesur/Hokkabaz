//! [`ChatRequest`] -> `/chat/completions` JSON body, shared by OpenAI-style
//! backends. Per-backend quirks live in [`BodyOptions`].

use serde_json::{Map, Value, json};

use crate::params::retain_supported;
use crate::{ChatRequest, Message, ReasoningEffort, Role, SamplingParams, ToolChoice};

/// Wire-format knobs that differ between OpenAI-style backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyOptions {
    /// Replay `reasoning_content` on assistant messages that carry tool calls
    /// (DeepSeek requires it within a tool-call sub-turn; OpenAI rejects it).
    pub echo_reasoning: bool,
    /// Ask for a trailing usage chunk (`stream_options.include_usage`).
    pub include_usage: bool,
    /// JSON field for the output cap: `max_tokens` (most servers) or
    /// `max_completion_tokens` (OpenAI o-series / newer models).
    pub max_tokens_field: String,
    /// Forward [`ChatRequest::reasoning_effort`] as `reasoning_effort`.
    pub send_reasoning_effort: bool,
    /// Model ids that reject sampling params (`temperature`, `top_p`,
    /// penalties, `logit_bias`); those are dropped for them.
    pub no_temperature_models: Vec<String>,
    /// Send the vLLM / llama.cpp extensions `top_k`, `min_p` and
    /// `repetition_penalty`. Off for strict OpenAI-style servers.
    pub extended_sampling: bool,
    /// Param keys (`AiSettings` field names) this backend never takes.
    pub drop_params: Vec<String>,
}

impl Default for BodyOptions {
    fn default() -> Self {
        Self {
            echo_reasoning: false,
            include_usage: true,
            max_tokens_field: "max_tokens".into(),
            send_reasoning_effort: true,
            no_temperature_models: Vec::new(),
            extended_sampling: false,
            drop_params: Vec::new(),
        }
    }
}

/// Whether a backend configured with `opts` sends param `key` for `model`.
pub(crate) fn supports(opts: &BodyOptions, model: &str, key: &str) -> bool {
    if opts.drop_params.iter().any(|p| p == key) {
        return false;
    }
    let sampling = !opts.no_temperature_models.iter().any(|m| m == model);
    match key {
        "temperature" | "top_p" | "presence_penalty" | "frequency_penalty" | "logit_bias" => {
            sampling
        }
        "top_k" | "min_p" | "repetition_penalty" => sampling && opts.extended_sampling,
        "max_output_tokens" | "stop" | "seed" | "json_mode" | "json_schema" => true,
        "reasoning_effort" => opts.send_reasoning_effort,
        // No OpenAI-style wire field for a token budget.
        _ => false,
    }
}

/// Build the streaming request body.
///
/// Model-visible means logged: everything here derives from `req` alone.
pub fn build_body_with(req: &ChatRequest, opts: &BodyOptions) -> Value {
    // DeepSeek shares this builder; the logged model id tells them apart.
    let req = &*retain_supported("openai_compat", req, |k| supports(opts, &req.model, k));
    let mut body = json!({
        "model": req.model,
        "messages": req.messages.iter().map(|m| message(m, opts.echo_reasoning)).collect::<Vec<_>>(),
        "stream": true,
    });
    let obj = body.as_object_mut().expect("json! object");
    if opts.include_usage {
        obj.insert("stream_options".into(), json!({ "include_usage": true }));
    }
    if let Some(t) = req.temperature {
        obj.insert("temperature".into(), json!(t));
    }
    if let Some(n) = req.max_output_tokens {
        obj.insert(opts.max_tokens_field.clone(), json!(n));
    }
    sampling(obj, &req.sampling);
    if let Some(e) = req.reasoning_effort {
        let e = match e {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
        };
        obj.insert("reasoning_effort".into(), json!(e));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({ "type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.parameters,
                }})
            })
            .collect();
        obj.insert("tools".into(), Value::Array(tools));
        obj.insert("tool_choice".into(), tool_choice(&req.tool_choice));
    }
    body
}

fn sampling(obj: &mut Map<String, Value>, p: &SamplingParams) {
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            obj.insert(k.into(), v);
        }
    };
    put("top_p", p.top_p.map(Value::from));
    put("top_k", p.top_k.map(Value::from));
    put("min_p", p.min_p.map(Value::from));
    put("presence_penalty", p.presence_penalty.map(Value::from));
    put("frequency_penalty", p.frequency_penalty.map(Value::from));
    put("repetition_penalty", p.repetition_penalty.map(Value::from));
    put("seed", p.seed.map(Value::from));
    put("stop", (!p.stop.is_empty()).then(|| json!(p.stop)));
    put(
        "logit_bias",
        (!p.logit_bias.is_empty()).then(|| json!(p.logit_bias)),
    );
    let format = match (&p.json_schema, p.json_mode) {
        (Some(schema), _) => Some(json!({ "type": "json_schema", "json_schema": {
            "name": "response", "schema": schema, "strict": true,
        }})),
        (None, true) => Some(json!({ "type": "json_object" })),
        (None, false) => None,
    };
    put("response_format", format);
}

fn tool_choice(c: &ToolChoice) -> Value {
    match c {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Named(n) => json!({ "type": "function", "function": { "name": n } }),
    }
}

fn message(m: &Message, echo_reasoning: bool) -> Value {
    let role = match m.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let mut v = json!({ "role": role, "content": m.content });
    let obj = v.as_object_mut().expect("json! object");
    if m.role == Role::Assistant && !m.tool_calls.is_empty() {
        if m.content.is_empty() {
            obj.insert("content".into(), Value::Null);
        }
        let calls: Vec<Value> = m
            .tool_calls
            .iter()
            .map(|c| {
                // Wire wants arguments as a JSON *string*.
                let args = match &c.arguments {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                json!({ "id": c.id, "type": "function",
                        "function": { "name": c.name, "arguments": args } })
            })
            .collect();
        obj.insert("tool_calls".into(), Value::Array(calls));
        if echo_reasoning && let Some(r) = &m.reasoning {
            obj.insert("reasoning_content".into(), json!(r));
        }
    }
    if let Some(id) = &m.tool_call_id {
        obj.insert("tool_call_id".into(), json!(id));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ToolCall, ToolSpec};

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.into(),
            reasoning: None,
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn tool_turn() -> ChatRequest {
        let mut asst = msg(Role::Assistant, "");
        asst.reasoning = Some("think".into());
        asst.tool_calls = vec![ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: json!({"path": "a"}),
        }];
        let mut tool = msg(Role::Tool, "ok");
        tool.tool_call_id = Some("c1".into());
        let mut req = ChatRequest::new("m", vec![msg(Role::User, "hi"), asst, tool]);
        req.temperature = Some(0.3);
        req.max_output_tokens = Some(100);
        req.reasoning_effort = Some(ReasoningEffort::High);
        req.tools = vec![ToolSpec {
            name: "read".into(),
            description: "d".into(),
            parameters: json!({"type": "object"}),
        }];
        req
    }

    #[test]
    fn default_shape_is_plain_openai() {
        let b = build_body_with(&tool_turn(), &BodyOptions::default());
        assert_eq!(b["stream_options"]["include_usage"], true);
        assert_eq!(b["max_tokens"], 100);
        assert_eq!(b["reasoning_effort"], "high");
        assert!((b["temperature"].as_f64().unwrap() - 0.3).abs() < 1e-6);
        let m = &b["messages"][1];
        assert_eq!(m["content"], Value::Null);
        assert!(m.get("reasoning_content").is_none());
        assert_eq!(
            m["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a\"}"
        );
        assert_eq!(b["messages"][2]["tool_call_id"], "c1");
        assert_eq!(b["tool_choice"], "auto");
    }

    #[test]
    fn options_change_wire_shape() {
        let opts = BodyOptions {
            echo_reasoning: true,
            include_usage: false,
            max_tokens_field: "max_completion_tokens".into(),
            send_reasoning_effort: false,
            no_temperature_models: vec!["m".into()],
            ..BodyOptions::default()
        };
        let b = build_body_with(&tool_turn(), &opts);
        assert!(b.get("stream_options").is_none());
        assert!(b.get("max_tokens").is_none());
        assert_eq!(b["max_completion_tokens"], 100);
        assert!(b.get("reasoning_effort").is_none());
        assert!(b.get("temperature").is_none());
        assert_eq!(b["messages"][1]["reasoning_content"], "think");
    }

    #[test]
    fn reasoning_dropped_without_tool_calls() {
        let mut a = msg(Role::Assistant, "answer");
        a.reasoning = Some("secret".into());
        let opts = BodyOptions {
            echo_reasoning: true,
            ..BodyOptions::default()
        };
        let b = build_body_with(&ChatRequest::new("m", vec![a]), &opts);
        assert!(b["messages"][0].get("reasoning_content").is_none());
    }
}
