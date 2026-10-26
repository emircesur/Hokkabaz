//! [`ChatRequest`] -> Anthropic `/v1/messages` JSON body.
//!
//! Wire differences from OpenAI-style APIs handled here: system prompt is a
//! top-level block list, tool results are `tool_result` blocks inside a user
//! message (parallel results share one message), `max_tokens` is mandatory,
//! thinking has two dialects, and cache breakpoints are explicit.

use serde_json::{Map, Value, json};

use super::store::{ReasoningStore, ThinkingBlock};
use crate::params::retain_supported;
use crate::{ChatRequest, Message, ReasoningEffort, Role, ToolChoice};

/// How `reasoning_effort` maps onto Anthropic's thinking parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingStyle {
    /// Pick by model id: pre-4.6 models use [`Self::Budget`], newer ones
    /// [`Self::Adaptive`].
    #[default]
    Auto,
    /// `thinking: {type: enabled, budget_tokens}` (Claude 3.7 - 4.5).
    Budget,
    /// `thinking: {type: adaptive}` + `output_config.effort` (4.6+; newer
    /// models reject `budget_tokens` and sampling params).
    Adaptive,
}

impl ThinkingStyle {
    fn resolve(self, model: &str) -> Self {
        match self {
            Self::Auto => {
                const LEGACY: [&str; 7] = [
                    "claude-3",
                    "-4-5",
                    "-4-1",
                    "claude-sonnet-4-0",
                    "claude-opus-4-0",
                    "claude-sonnet-4-2025",
                    "claude-opus-4-2025",
                ];
                if LEGACY.iter().any(|p| model.contains(p)) {
                    Self::Budget
                } else {
                    Self::Adaptive
                }
            }
            other => other,
        }
    }
}

/// Adapter-level request defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestOptions {
    /// Cache breakpoints when [`ChatRequest::prompt_cache`] is `None`.
    pub prompt_cache: bool,
    /// `max_tokens` when the request sets no cap (the API requires one).
    pub default_max_tokens: u64,
    /// Thinking dialect override.
    pub thinking_style: ThinkingStyle,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            prompt_cache: true,
            default_max_tokens: 16_000,
            thinking_style: ThinkingStyle::Auto,
        }
    }
}

enum Think {
    Off,
    Budget(u64),
    Adaptive(&'static str),
}

const MIN_BUDGET: u64 = 1024;
/// Headroom above the thinking budget for the visible answer.
const ANSWER_ROOM: u64 = 8192;

fn budget_for(e: ReasoningEffort) -> u64 {
    match e {
        ReasoningEffort::Low => 2048,
        ReasoningEffort::Medium => 8192,
        ReasoningEffort::High => 24_576,
    }
}

fn effort_name(e: ReasoningEffort) -> &'static str {
    match e {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
    }
}

/// True when every assistant tool turn after the last user message has stored
/// thinking blocks, i.e. thinking may stay on without breaking the history.
fn replayable(messages: &[Message], store: &ReasoningStore) -> bool {
    let start = messages
        .iter()
        .rposition(|m| m.role == Role::User)
        .map_or(0, |i| i + 1);
    messages[start..]
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .filter_map(|m| m.tool_calls.first())
        .all(|c| store.get(&c.id).is_some())
}

fn plan(req: &ChatRequest, style: ThinkingStyle, max_tokens: Option<u64>) -> Think {
    let budget = req.sampling.thinking_budget;
    if budget == Some(0) || (budget.is_none() && req.reasoning_effort.is_none()) {
        return Think::Off;
    }
    // Thinking rejects forced tool choice.
    if !req.tools.is_empty()
        && matches!(req.tool_choice, ToolChoice::Required | ToolChoice::Named(_))
    {
        return Think::Off;
    }
    match style {
        ThinkingStyle::Adaptive | ThinkingStyle::Auto => match req.reasoning_effort {
            Some(e) => Think::Adaptive(effort_name(e)),
            None => Think::Off,
        },
        ThinkingStyle::Budget => {
            // An explicit budget wins over the effort preset.
            let mut b =
                budget.map_or_else(|| req.reasoning_effort.map_or(0, budget_for), u64::from);
            if let Some(cap) = max_tokens {
                b = b.min(cap.saturating_sub(1));
            }
            if b < MIN_BUDGET {
                Think::Off
            } else {
                Think::Budget(b)
            }
        }
    }
}

/// Whether param `key` is sent for a model of (resolved) `style`. Budget-style
/// models take sampling params only with thinking off; adaptive (4.6+) models
/// reject sampling and `budget_tokens` outright. JSON goes through
/// `output_config.format`, which needs a schema; bare JSON mode has no wire form.
pub(crate) fn supports(style: ThinkingStyle, thinking: bool, key: &str) -> bool {
    let budget = style == ThinkingStyle::Budget;
    match key {
        "max_output_tokens" | "stop" | "reasoning_effort" | "json_schema" => true,
        "temperature" | "top_p" | "top_k" => budget && !thinking,
        "thinking_budget" => budget,
        _ => false,
    }
}

/// [`supports`] for a request that does not ask for thinking (UI view).
pub(crate) fn supports_static(model: &str, key: &str) -> bool {
    supports(ThinkingStyle::Auto.resolve(model), false, key)
}

fn wants_thinking(req: &ChatRequest) -> bool {
    match req.sampling.thinking_budget {
        Some(b) => b > 0,
        None => req.reasoning_effort.is_some(),
    }
}

/// Build the streaming request body.
///
/// Model-visible means logged: all content derives from `req`; `store` only
/// supplies the signatures needed to replay thinking blocks.
pub(crate) fn build_body(
    req: &ChatRequest,
    opts: &RequestOptions,
    store: &ReasoningStore,
) -> Value {
    let cache = req.prompt_cache.unwrap_or(opts.prompt_cache);
    let style = opts.thinking_style.resolve(&req.model);
    let thinking = wants_thinking(req);
    let req = &*retain_supported("anthropic", req, |k| supports(style, thinking, k));

    let mut think = plan(req, style, req.max_output_tokens);
    if !matches!(think, Think::Off) && !replayable(&req.messages, store) {
        think = Think::Off;
    }
    let max_tokens = req.max_output_tokens.unwrap_or(match think {
        Think::Budget(b) => opts.default_max_tokens.max(b + ANSWER_ROOM),
        _ => opts.default_max_tokens,
    });

    let (system, messages) = convert(&req.messages, !matches!(think, Think::Off), store, cache);
    let mut body = json!({
        "model": req.model,
        "max_tokens": max_tokens,
        "messages": messages,
        "stream": true,
    });
    let obj = body.as_object_mut().expect("json! object");
    if !system.is_empty() {
        obj.insert("system".into(), Value::Array(system));
    }
    // Sampling params survive `retain_supported` only for budget-style models
    // without thinking requested.
    if let Some(t) = req.temperature {
        obj.insert("temperature".into(), json!(t));
    }
    if let Some(p) = req.sampling.top_p {
        obj.insert("top_p".into(), json!(p));
    }
    if let Some(k) = req.sampling.top_k {
        obj.insert("top_k".into(), json!(k));
    }
    if !req.sampling.stop.is_empty() {
        obj.insert("stop_sequences".into(), json!(req.sampling.stop));
    }
    let mut output_config = Map::new();
    if let Some(schema) = &req.sampling.json_schema {
        output_config.insert(
            "format".into(),
            json!({ "type": "json_schema", "schema": schema }),
        );
    }
    match think {
        Think::Off => {}
        Think::Budget(b) => {
            obj.insert(
                "thinking".into(),
                json!({ "type": "enabled", "budget_tokens": b }),
            );
        }
        Think::Adaptive(effort) => {
            // `summarized` so ReasoningDelta carries text (default is empty).
            obj.insert(
                "thinking".into(),
                json!({ "type": "adaptive", "display": "summarized" }),
            );
            output_config.insert("effort".into(), json!(effort));
        }
    }
    if !output_config.is_empty() {
        obj.insert("output_config".into(), Value::Object(output_config));
    }
    if !req.tools.is_empty() {
        let n = req.tools.len();
        let tools: Vec<Value> = req
            .tools
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let mut v = json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                });
                if cache && i + 1 == n {
                    v["cache_control"] = ephemeral();
                }
                v
            })
            .collect();
        obj.insert("tools".into(), Value::Array(tools));
        obj.insert("tool_choice".into(), tool_choice(&req.tool_choice));
    }
    body
}

fn ephemeral() -> Value {
    json!({ "type": "ephemeral" })
}

fn tool_choice(c: &ToolChoice) -> Value {
    match c {
        ToolChoice::Auto => json!({ "type": "auto" }),
        ToolChoice::None => json!({ "type": "none" }),
        ToolChoice::Required => json!({ "type": "any" }),
        ToolChoice::Named(n) => json!({ "type": "tool", "name": n }),
    }
}

/// Text block; the API rejects empty/whitespace-only text.
fn text_block(s: &str) -> Option<Value> {
    (!s.trim().is_empty()).then(|| json!({ "type": "text", "text": s }))
}

/// User-side content being accumulated. `tool_result` blocks must precede
/// other blocks in a user message.
#[derive(Default)]
struct UserTurn {
    results: Vec<Value>,
    other: Vec<Value>,
}

impl UserTurn {
    fn flush(&mut self, out: &mut Vec<Value>) {
        if self.results.is_empty() && self.other.is_empty() {
            return;
        }
        let mut content = std::mem::take(&mut self.results);
        content.append(&mut self.other);
        out.push(json!({ "role": "user", "content": content }));
    }
}

fn convert(
    messages: &[Message],
    thinking_on: bool,
    store: &ReasoningStore,
    cache: bool,
) -> (Vec<Value>, Vec<Value>) {
    let mut system = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    let mut user = UserTurn::default();

    for m in messages {
        match m.role {
            Role::System => system.extend(text_block(&m.content)),
            Role::User => {
                for i in &m.images {
                    user.other.push(json!({
                        "type": "image",
                        "source": {"type": "base64", "media_type": i.mime, "data": i.data},
                    }));
                }
                // Never drop a user turn: keep alternation valid.
                user.other.push(
                    text_block(&m.content)
                        .unwrap_or_else(|| json!({ "type": "text", "text": "(empty)" })),
                );
            }
            Role::Tool => user.results.push(json!({
                "type": "tool_result",
                "tool_use_id": m.tool_call_id.as_deref().unwrap_or_default(),
                "content": m.content,
            })),
            Role::Assistant => {
                user.flush(&mut out);
                let mut content: Vec<Value> = Vec::new();
                if thinking_on
                    && let Some(first) = m.tool_calls.first()
                    && let Some(blocks) = store.get(&first.id)
                {
                    content.extend(blocks.iter().map(|b| match b {
                        ThinkingBlock::Thinking {
                            thinking,
                            signature,
                        } => json!({
                            "type": "thinking",
                            "thinking": thinking,
                            "signature": signature,
                        }),
                        ThinkingBlock::Redacted { data } => {
                            json!({ "type": "redacted_thinking", "data": data })
                        }
                    }));
                }
                content.extend(text_block(&m.content));
                content.extend(m.tool_calls.iter().map(|c| {
                    json!({
                        "type": "tool_use",
                        "id": c.id,
                        "name": c.name,
                        "input": if c.arguments.is_null() { json!({}) } else { c.arguments.clone() },
                    })
                }));
                if !content.is_empty() {
                    out.push(json!({ "role": "assistant", "content": content }));
                }
            }
        }
    }
    user.flush(&mut out);

    if cache {
        // Breakpoint on the last block of the final user message: the whole
        // conversation prefix up to here is cached for the next step.
        if let Some(last) = out.last_mut()
            && last["role"] == "user"
            && let Some(block) = last["content"].as_array_mut().and_then(|a| a.last_mut())
        {
            block["cache_control"] = ephemeral();
        }
        if let Some(block) = system.last_mut() {
            block["cache_control"] = ephemeral();
        }
    }
    (system, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ToolCall, ToolSpec};

    fn msg(role: Role, content: &str) -> Message {
        Message {
            images: Vec::new(),
            role,
            content: content.into(),
            reasoning: None,
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    fn tool_turn() -> Vec<Message> {
        let mut a = msg(Role::Assistant, "");
        a.tool_calls = vec![
            ToolCall {
                id: "toolu_1".into(),
                name: "ls".into(),
                arguments: json!({"p": "."}),
            },
            ToolCall {
                id: "toolu_2".into(),
                name: "cat".into(),
                arguments: json!({"p": "a"}),
            },
        ];
        let mut t1 = msg(Role::Tool, "a b");
        t1.tool_call_id = Some("toolu_1".into());
        let mut t2 = msg(Role::Tool, "hello");
        t2.tool_call_id = Some("toolu_2".into());
        vec![
            msg(Role::System, "be brief"),
            msg(Role::User, "go"),
            a,
            t1,
            t2,
        ]
    }

    fn tools() -> Vec<ToolSpec> {
        ["ls", "cat"]
            .iter()
            .map(|n| ToolSpec {
                name: (*n).into(),
                description: "d".into(),
                parameters: json!({"type": "object"}),
            })
            .collect()
    }

    fn body(req: &ChatRequest, store: &ReasoningStore) -> Value {
        build_body(req, &RequestOptions::default(), store)
    }

    #[test]
    fn anthropic_body_merges_tool_results_and_places_cache_breakpoints() {
        let mut req = ChatRequest::new("claude-sonnet-4-5", tool_turn());
        req.tools = tools();
        let b = body(&req, &ReasoningStore::default());
        assert_eq!(b["system"][0]["text"], "be brief");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["tools"][0].get("cache_control"), None);
        assert_eq!(b["tools"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["tool_choice"]["type"], "auto");
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m.len(), 3, "user, assistant, merged tool results");
        assert_eq!(m[1]["content"][0]["type"], "tool_use");
        assert_eq!(m[1]["content"][1]["id"], "toolu_2");
        let results = m[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].get("cache_control"), None);
        assert_eq!(results[1]["tool_use_id"], "toolu_2");
        assert_eq!(results[1]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["max_tokens"], 16_000);
        assert_eq!(b["stream"], true);
    }

    #[test]
    fn anthropic_cache_off_per_request() {
        let mut req = ChatRequest::new("claude-sonnet-4-5", tool_turn());
        req.tools = tools();
        req.prompt_cache = Some(false);
        let s = body(&req, &ReasoningStore::default()).to_string();
        assert!(!s.contains("cache_control"), "{s}");
    }

    #[test]
    fn anthropic_budget_thinking_replays_signed_blocks() {
        let store = ReasoningStore::default();
        store.put(
            &["toolu_1".into(), "toolu_2".into()],
            vec![ThinkingBlock::Thinking {
                thinking: "hm".into(),
                signature: "sig".into(),
            }],
        );
        let mut req = ChatRequest::new("claude-sonnet-4-5", tool_turn());
        req.tools = tools();
        req.reasoning_effort = Some(ReasoningEffort::Low);
        req.temperature = Some(0.2);
        let b = body(&req, &store);
        assert_eq!(b["thinking"]["type"], "enabled");
        assert_eq!(b["thinking"]["budget_tokens"], 2048);
        assert_eq!(
            b.get("temperature"),
            None,
            "thinking requires default sampling"
        );
        let first = &b["messages"][1]["content"][0];
        assert_eq!(first["type"], "thinking");
        assert_eq!(first["signature"], "sig");
        assert_eq!(b["messages"][1]["content"][1]["type"], "tool_use");
    }

    #[test]
    fn anthropic_thinking_off_when_turn_cannot_be_replayed() {
        let mut req = ChatRequest::new("claude-sonnet-4-5", tool_turn());
        req.reasoning_effort = Some(ReasoningEffort::High);
        let b = body(&req, &ReasoningStore::default());
        assert_eq!(b.get("thinking"), None);
        assert_eq!(b["messages"][1]["content"][0]["type"], "tool_use");
        // Fresh user turn: nothing to replay, thinking stays on.
        let mut fresh = ChatRequest::new("claude-sonnet-4-5", vec![msg(Role::User, "hi")]);
        fresh.reasoning_effort = Some(ReasoningEffort::High);
        assert_eq!(
            body(&fresh, &ReasoningStore::default())["thinking"]["budget_tokens"],
            24_576
        );
    }

    #[test]
    fn anthropic_adaptive_merges_effort_and_format_drops_budget() {
        let mut req = ChatRequest::new("claude-opus-5-5", vec![msg(Role::User, "hi")]);
        req.reasoning_effort = Some(ReasoningEffort::High);
        req.sampling.thinking_budget = Some(4096);
        req.sampling.top_k = Some(5);
        req.sampling.json_schema = Some(json!({"type": "object"}));
        let b = body(&req, &ReasoningStore::default());
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert_eq!(b["output_config"]["effort"], "high");
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert!(b.get("top_k").is_none());

        // Budget style without thinking keeps sampling; zero budget = off.
        let mut req = ChatRequest::new("claude-sonnet-4-5", vec![msg(Role::User, "hi")]);
        req.sampling.thinking_budget = Some(0);
        req.sampling.top_k = Some(5);
        req.temperature = Some(0.5);
        let b = body(&req, &ReasoningStore::default());
        assert!(b.get("thinking").is_none());
        assert_eq!(b["top_k"], 5);
        assert_eq!(b["temperature"], 0.5);
    }

    #[test]
    fn anthropic_adaptive_style_for_new_models_drops_sampling() {
        let mut req = ChatRequest::new("claude-opus-5-5", vec![msg(Role::User, "hi")]);
        req.reasoning_effort = Some(ReasoningEffort::Medium);
        req.temperature = Some(0.5);
        let b = body(&req, &ReasoningStore::default());
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert_eq!(b["output_config"]["effort"], "medium");
        assert_eq!(b.get("temperature"), None);
        assert_eq!(b.get("budget_tokens"), None);
    }

    #[test]
    fn anthropic_forced_tool_choice_disables_thinking_and_budget_clamps() {
        let mut req = ChatRequest::new("claude-haiku-4-5", vec![msg(Role::User, "hi")]);
        req.tools = tools();
        req.tool_choice = ToolChoice::Named("ls".into());
        req.reasoning_effort = Some(ReasoningEffort::High);
        let b = body(&req, &ReasoningStore::default());
        assert_eq!(b.get("thinking"), None);
        assert_eq!(b["tool_choice"], json!({"type": "tool", "name": "ls"}));

        let mut req = ChatRequest::new("claude-haiku-4-5", vec![msg(Role::User, "hi")]);
        req.reasoning_effort = Some(ReasoningEffort::High);
        req.max_output_tokens = Some(4000);
        let b = body(&req, &ReasoningStore::default());
        assert_eq!(b["thinking"]["budget_tokens"], 3999);
        req.max_output_tokens = Some(1000);
        assert_eq!(body(&req, &ReasoningStore::default()).get("thinking"), None);
    }
}
