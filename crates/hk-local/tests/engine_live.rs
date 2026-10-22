//! Runs a real GGUF through the engine. Skipped unless `HK_LOCAL_GGUF` points at a model, e.g.
//! `HK_LOCAL_GGUF=/path/qwen2.5-0.5b-instruct-q4_k_m.gguf cargo test -p hk-local --features llama --test engine_live -- --nocapture`
#![cfg(feature = "llama")]

use futures::StreamExt;
use hk_llm::{ChatRequest, FinishReason, Message, Provider, Role, StreamEvent, ToolSpec};
use hk_local::{ModelStore, engine::LlamaProvider};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn msg(role: Role, content: &str) -> Message {
    Message {
        role,
        content: content.into(),
        reasoning: None,
        tool_calls: vec![],
        tool_call_id: None,
    }
}

#[tokio::test]
async fn answers_and_calls_tools() {
    let Some(path) = std::env::var_os("HK_LOCAL_GGUF") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let dir = std::env::temp_dir().join(format!("hk-local-live-{}", std::process::id()));
    let store = ModelStore::new(&dir);
    let m = store.add_custom(&path, Some("live")).unwrap();
    let p = LlamaProvider::new(store);
    assert_eq!(p.models().await.unwrap().len(), 1);

    let mut req = ChatRequest::new(
        m.id.clone(),
        vec![
            msg(Role::System, "Answer in one short sentence."),
            msg(Role::User, "What is the capital of France?"),
        ],
    );
    req.temperature = Some(0.0);
    req.max_output_tokens = Some(48);
    let mut s = p.stream(req, CancellationToken::new()).await.unwrap();
    let (mut text, mut fin) = (String::new(), None);
    while let Some(ev) = s.next().await {
        match ev.unwrap() {
            StreamEvent::TextDelta { text: t } => text.push_str(&t),
            StreamEvent::Finish(f) => fin = Some(f),
            _ => {}
        }
    }
    eprintln!("answer: {text}");
    assert!(text.to_lowercase().contains("paris"), "{text}");
    assert!(fin.is_some());

    let mut req = ChatRequest::new(
        m.id,
        vec![
            msg(
                Role::System,
                "You are an assistant with tools. Use a tool whenever one fits.",
            ),
            msg(Role::User, "Read the file notes.txt"),
        ],
    );
    req.tools.push(ToolSpec {
        name: "read_file".into(),
        description: "Read a text file from disk".into(),
        parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    });
    req.temperature = Some(0.0);
    req.max_output_tokens = Some(96);
    let mut s = p.stream(req, CancellationToken::new()).await.unwrap();
    let (mut name, mut args, mut fin) = (None, String::new(), None);
    while let Some(ev) = s.next().await {
        match ev.unwrap() {
            StreamEvent::ToolCallDelta {
                name: n,
                arguments_delta,
                ..
            } => {
                name = name.or(n);
                args.push_str(&arguments_delta);
            }
            StreamEvent::Finish(f) => fin = Some(f),
            _ => {}
        }
    }
    eprintln!("tool: {name:?} {args}");
    assert_eq!(name.as_deref(), Some("read_file"));
    assert!(args.contains("notes.txt"));
    assert_eq!(fin, Some(FinishReason::ToolCalls));
    let _ = std::fs::remove_dir_all(dir);
}
