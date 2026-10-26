//! Request-body snapshots: one fully-populated settings stack mapped through
//! every adapter. Each snapshot shows which knobs survive and their wire names.
//! Regenerate with `UPDATE_SNAPSHOTS=1 cargo test -p hk-llm --test settings_bodies`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use hk_core::ai_settings::{
    AiSettings, ReasoningEffort, ReasoningVisibility, Scope, SettingsStack,
};
use hk_llm::{ChatRequest, Message, Role, anthropic, deepseek, gemini, ollama, openai};
use serde_json::{Value, json};

fn msg(role: Role, content: &str) -> Message {
    Message { images: Vec::new(),
        role,
        content: content.into(),
        reasoning: None,
        tool_calls: vec![],
        tool_call_id: None,
    }
}

/// Every model param set, split over two layers, plus harness-only fields
/// that must never reach the wire.
fn req(model: &str) -> ChatRequest {
    let mut stack = SettingsStack::new();
    stack.push(
        Scope::Global,
        AiSettings {
            temperature: Some(1.0),
            top_p: Some(0.9),
            top_k: Some(40),
            min_p: Some(0.05),
            presence_penalty: Some(0.1),
            frequency_penalty: Some(0.2),
            repetition_penalty: Some(1.1),
            seed: Some(42),
            stop: Some(vec!["</done>".into()]),
            logit_bias: Some(BTreeMap::from([("50256".into(), -100.0)])),
            show_reasoning: Some(ReasoningVisibility::Collapse),
            compaction_threshold: Some(0.7),
            ..Default::default()
        },
    );
    stack.push(
        Scope::Session("s1".into()),
        AiSettings {
            temperature: Some(0.5),
            max_output_tokens: Some(8192),
            reasoning_effort: Some(ReasoningEffort::High),
            thinking_budget: Some(4096),
            json_mode: Some(true),
            json_schema: Some(json!({
                "type": "object",
                "properties": { "answer": { "type": "string" } },
                "required": ["answer"]
            })),
            stream: Some(true),
            ..Default::default()
        },
    );
    ChatRequest::new(
        model,
        vec![msg(Role::System, "Be terse."), msg(Role::User, "Hi")],
    )
    .with_settings(&stack.resolved())
}

fn check(name: &str, body: &Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("settings_body_{name}.json"));
    let actual = serde_json::to_string_pretty(body).unwrap() + "\n";
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_SNAPSHOTS=1)", path.display()));
    let expected: Value = serde_json::from_str(&expected).unwrap();
    assert_eq!(body, &expected, "snapshot mismatch: {}", path.display());
}

#[test]
fn deepseek_body_snapshot() {
    // Chat model: no seed / logit_bias / schema / effort; schema falls back to json_object.
    check("deepseek", &deepseek::build_body(&req("deepseek-chat")));
}

#[test]
fn openai_compat_body_snapshot() {
    // vLLM-style server: extended sampling on, no token budget.
    let opts = openai::BodyOptions {
        extended_sampling: true,
        ..openai::BodyOptions::default()
    };
    check("openai", &openai::build_body_with(&req("qwen3-32b"), &opts));
}

#[test]
fn anthropic_body_snapshot() {
    // Budget-style model with thinking: sampling dropped, budget_tokens kept.
    let opts = anthropic::RequestOptions {
        prompt_cache: false,
        ..anthropic::RequestOptions::default()
    };
    check(
        "anthropic",
        &anthropic::build_body(&req("claude-sonnet-4-5"), &opts),
    );
}

#[test]
fn gemini_body_snapshot() {
    check("gemini", &gemini::build_body(&req("gemini-2.5-flash")));
}

#[test]
fn ollama_body_snapshot() {
    check(
        "ollama",
        &ollama::build_body(&req("qwen3:8b"), &ollama::BodyOptions::default()),
    );
}
