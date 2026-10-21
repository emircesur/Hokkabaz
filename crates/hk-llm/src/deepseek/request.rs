//! [`ChatRequest`] -> DeepSeek `/chat/completions` JSON body.

use serde_json::{Value, json};

use crate::{ChatRequest, Message, Role, ToolChoice};

/// Build the streaming request body.
///
/// DeepSeek specifics:
/// - `reasoning_content` is sent back only on assistant messages that carry
///   tool calls (the API requires it within a tool-call sub-turn and drops or
///   rejects it elsewhere).
/// - `deepseek-reasoner` ignores sampling params, so `temperature` is omitted.
/// - `stream_options.include_usage` is always on: usage + cache-hit counters
///   arrive in a trailing chunk.
pub fn build_body(req: &ChatRequest) -> Value {
    let mut body = json!({
        "model": req.model,
        "messages": req.messages.iter().map(message).collect::<Vec<_>>(),
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    let obj = body.as_object_mut().expect("json! object");
    if let Some(t) = req.temperature
        && req.model != "deepseek-reasoner"
    {
        obj.insert("temperature".into(), json!(t));
    }
    if let Some(n) = req.max_output_tokens {
        obj.insert("max_tokens".into(), json!(n));
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

fn tool_choice(c: &ToolChoice) -> Value {
    match c {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Named(n) => json!({ "type": "function", "function": { "name": n } }),
    }
}

fn message(m: &Message) -> Value {
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
        if let Some(r) = &m.reasoning {
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

    #[test]
    fn tool_turn_roundtrip_shape() {
        let mut asst = msg(Role::Assistant, "");
        asst.reasoning = Some("think".into());
        asst.tool_calls = vec![ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: json!({"path": "a"}),
        }];
        let mut tool = msg(Role::Tool, "ok");
        tool.tool_call_id = Some("c1".into());
        let mut req =
            ChatRequest::new("deepseek-reasoner", vec![msg(Role::User, "hi"), asst, tool]);
        req.temperature = Some(0.3);
        req.tools = vec![ToolSpec {
            name: "read".into(),
            description: "d".into(),
            parameters: json!({"type": "object"}),
        }];
        let b = build_body(&req);
        assert!(b.get("temperature").is_none());
        assert_eq!(b["stream_options"]["include_usage"], true);
        let m = &b["messages"][1];
        assert_eq!(m["content"], Value::Null);
        assert_eq!(m["reasoning_content"], "think");
        assert_eq!(
            m["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a\"}"
        );
        assert_eq!(b["messages"][2]["tool_call_id"], "c1");
        assert_eq!(b["tool_choice"], "auto");
    }

    #[test]
    fn reasoning_dropped_without_tool_calls() {
        let mut a = msg(Role::Assistant, "answer");
        a.reasoning = Some("secret".into());
        let b = build_body(&ChatRequest::new("deepseek-chat", vec![a]));
        assert!(b["messages"][0].get("reasoning_content").is_none());
    }
}
