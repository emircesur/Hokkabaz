//! [`ChatRequest`] -> Ollama `/api/chat` body.

use serde_json::{Map, Value, json};

use crate::{ChatRequest, Message, Role, ToolChoice};

/// Ollama-specific knobs that have no place in the neutral request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BodyOptions {
    /// How long the server keeps the model loaded after the request
    /// (`"5m"`, `"0"` to unload, `"-1"` forever). `None` = server default.
    pub keep_alive: Option<String>,
    /// Context window (`options.num_ctx`). Ollama defaults to a small window
    /// and silently truncates the prompt, so callers usually want to set it.
    pub num_ctx: Option<u64>,
}

/// Build the streaming chat body.
///
/// Model-visible means logged: everything here derives from `req` + `opts`.
pub fn build_body(req: &ChatRequest, opts: &BodyOptions) -> Value {
    let mut body = json!({
        "model": req.model,
        "messages": messages(&req.messages),
        "stream": true,
    });
    let obj = body.as_object_mut().expect("json! object");

    let mut options = Map::new();
    if let Some(t) = req.temperature {
        options.insert("temperature".into(), json!(t));
    }
    if let Some(n) = req.max_output_tokens {
        options.insert("num_predict".into(), json!(n));
    }
    if let Some(n) = opts.num_ctx {
        options.insert("num_ctx".into(), json!(n));
    }
    if !options.is_empty() {
        obj.insert("options".into(), Value::Object(options));
    }
    if let Some(k) = &opts.keep_alive {
        obj.insert("keep_alive".into(), json!(k));
    }
    // Models without thinking support ignore/reject this only when it is set,
    // so send it only when the caller asked for reasoning.
    if req.reasoning_effort.is_some() {
        obj.insert("think".into(), json!(true));
    }

    // No native tool_choice: `None` drops the tools, `Named` narrows the
    // offered set, `Required` is best-effort (model decides).
    let tools: Vec<Value> = match &req.tool_choice {
        ToolChoice::None => Vec::new(),
        ToolChoice::Named(n) => req
            .tools
            .iter()
            .filter(|t| &t.name == n)
            .map(tool)
            .collect(),
        ToolChoice::Auto | ToolChoice::Required => req.tools.iter().map(tool).collect(),
    };
    if !tools.is_empty() {
        obj.insert("tools".into(), Value::Array(tools));
    }
    body
}

fn tool(t: &crate::ToolSpec) -> Value {
    json!({ "type": "function", "function": {
        "name": t.name, "description": t.description, "parameters": t.parameters,
    }})
}

fn messages(msgs: &[Message]) -> Vec<Value> {
    msgs.iter()
        .enumerate()
        .map(|(i, m)| message(m, &msgs[..i]))
        .collect()
}

fn message(m: &Message, before: &[Message]) -> Value {
    let role = match m.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let mut v = json!({ "role": role, "content": m.content });
    let obj = v.as_object_mut().expect("json! object");
    if !m.tool_calls.is_empty() {
        let calls: Vec<Value> = m
            .tool_calls
            .iter()
            .map(|c| json!({ "function": { "name": c.name, "arguments": c.arguments } }))
            .collect();
        obj.insert("tool_calls".into(), Value::Array(calls));
    }
    if m.role == Role::Tool
        && let Some(id) = &m.tool_call_id
    {
        // Ollama matches results by tool name, not id.
        let name = before
            .iter()
            .rev()
            .flat_map(|p| p.tool_calls.iter())
            .find(|c| &c.id == id)
            .map(|c| c.name.clone());
        if let Some(n) = name {
            obj.insert("tool_name".into(), json!(n));
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ReasoningEffort, ToolCall, ToolSpec};

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
    fn tools_options_and_tool_result_name() {
        let mut asst = msg(Role::Assistant, "");
        asst.tool_calls = vec![ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: json!({"path": "a.rs"}),
        }];
        let mut res = msg(Role::Tool, "fn main(){}");
        res.tool_call_id = Some("c1".into());
        let mut req = ChatRequest::new("qwen3:8b", vec![msg(Role::User, "hi"), asst, res]);
        req.temperature = Some(0.2);
        req.max_output_tokens = Some(64);
        req.reasoning_effort = Some(ReasoningEffort::Low);
        req.tools = vec![
            ToolSpec {
                name: "read".into(),
                description: "d".into(),
                parameters: json!({"type":"object"}),
            },
            ToolSpec {
                name: "glob".into(),
                description: "d".into(),
                parameters: json!({"type":"object"}),
            },
        ];
        let opts = BodyOptions {
            keep_alive: Some("10m".into()),
            num_ctx: Some(8192),
        };
        let b = build_body(&req, &opts);
        assert_eq!(b["stream"], true);
        assert_eq!(b["think"], true);
        assert_eq!(b["keep_alive"], "10m");
        assert_eq!(b["options"]["num_ctx"], 8192);
        assert_eq!(b["options"]["num_predict"], 64);
        assert_eq!(b["tools"].as_array().unwrap().len(), 2);
        assert_eq!(
            b["messages"][1]["tool_calls"][0]["function"]["arguments"]["path"],
            "a.rs"
        );
        assert_eq!(b["messages"][2]["tool_name"], "read");

        req.tool_choice = ToolChoice::Named("glob".into());
        let b = build_body(&req, &opts);
        assert_eq!(b["tools"].as_array().unwrap().len(), 1);
        req.tool_choice = ToolChoice::None;
        assert!(build_body(&req, &opts).get("tools").is_none());
    }
}
