//! Anthropic adapter against recorded fixtures. No live API.
//!
//! Fixture format: one SSE `data:` payload per line (`event:` lines omitted;
//! the payload's own `type` is authoritative).

use futures::StreamExt;
use hk_llm::anthropic::Anthropic;
use hk_llm::openai::RetryPolicy;
use hk_llm::{
    CancellationToken, ChatRequest, FinishReason, LlmError, Message, Provider, ReasoningEffort,
    Role, StreamEvent, ToolCall, Usage,
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn fixture(name: &str) -> String {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(p).expect("fixture")
}

fn sse(name: &str) -> String {
    // Real streams interleave `event:` lines; the decoder must skip them.
    let mut out = String::new();
    for l in fixture(name).lines().filter(|l| !l.is_empty()) {
        let ty = serde_json::from_str::<Value>(l).unwrap()["type"]
            .as_str()
            .unwrap()
            .to_owned();
        out.push_str(&format!("event: {ty}\ndata: {l}\n\n"));
    }
    out
}

/// Sequential one-shot HTTP/1.1 server; returns base URL and raw requests.
async fn mock_server(
    responses: Vec<String>,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        for r in responses {
            let (mut sock, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 32768];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
            sock.write_all(r.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), rx)
}

fn http(status: &str, ct: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn ok(name: &str) -> String {
    http("200 OK", "text/event-stream", &sse(name))
}

fn adapter(base: String, retries: u32) -> Anthropic {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    Anthropic::with_client(client, "sk-ant-secret")
        .base_url(base)
        .retry(RetryPolicy {
            max_retries: retries,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
        })
}

fn user(text: &str) -> Message {
    Message { images: Vec::new(),
        role: Role::User,
        content: text.into(),
        reasoning: None,
        tool_calls: vec![],
        tool_call_id: None,
    }
}

fn req() -> ChatRequest {
    ChatRequest::new("claude-sonnet-4-5", vec![user("hi")])
}

async fn run(p: &Anthropic, r: ChatRequest) -> Vec<Result<StreamEvent, LlmError>> {
    p.stream(r, CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await
}

async fn events(p: &Anthropic, r: ChatRequest) -> Vec<StreamEvent> {
    run(p, r).await.into_iter().map(|e| e.unwrap()).collect()
}

fn usage(i: u64, o: u64, r: u64, w: u64) -> StreamEvent {
    StreamEvent::Usage(Usage {
        input_tokens: i,
        output_tokens: o,
        cache_read_tokens: r,
        cache_write_tokens: w,
        reasoning_tokens: 0,
    })
}

#[tokio::test]
async fn anthropic_text_stream_and_request_shape() {
    let (base, mut seen) = mock_server(vec![ok("anthropic_text.jsonl")]).await;
    let p = adapter(base, 0);
    assert_eq!(p.id(), "anthropic");
    assert_eq!(
        events(&p, req()).await,
        vec![
            usage(25, 1, 0, 0),
            StreamEvent::TextDelta {
                text: "Hello".into()
            },
            StreamEvent::TextDelta {
                text: ", dünya".into()
            },
            usage(25, 7, 0, 0),
            StreamEvent::Finish(FinishReason::Stop),
        ]
    );
    let raw = seen.recv().await.unwrap().to_ascii_lowercase();
    assert!(raw.starts_with("post /v1/messages "), "{raw}");
    assert!(raw.contains("x-api-key: sk-ant-secret"));
    assert!(raw.contains("anthropic-version: 2023-06-01"));
    assert!(!raw.contains("authorization"));
    assert!(raw.contains("\"stream\":true") && raw.contains("\"max_tokens\":16000"));
}

#[tokio::test]
async fn anthropic_thinking_maps_to_reasoning_deltas() {
    let (base, _rx) = mock_server(vec![ok("anthropic_thinking.jsonl")]).await;
    assert_eq!(
        events(&adapter(base, 0), req()).await,
        vec![
            usage(30, 2, 0, 0),
            StreamEvent::ReasoningDelta {
                text: "Let me ".into()
            },
            StreamEvent::ReasoningDelta {
                text: "think.".into()
            },
            StreamEvent::TextDelta { text: "42".into() },
            usage(30, 40, 0, 0),
            StreamEvent::Finish(FinishReason::Stop),
        ]
    );
}

#[tokio::test]
async fn anthropic_tool_calls_assemble_and_signature_is_replayed() {
    let follow_up = ok("anthropic_text.jsonl");
    let (base, mut seen) = mock_server(vec![ok("anthropic_tool_calls.jsonl"), follow_up]).await;
    let p = adapter(base, 0);
    let mut r = req();
    r.reasoning_effort = Some(ReasoningEffort::Low);
    let evs = events(&p, r).await;

    let mut args = [String::new(), String::new()];
    let mut ids = [None, None];
    let mut names = [None, None];
    for e in &evs {
        if let StreamEvent::ToolCallDelta {
            index,
            id,
            name,
            arguments_delta,
        } = e
        {
            let i = *index as usize;
            args[i].push_str(arguments_delta);
            ids[i] = ids[i].take().or(id.clone());
            names[i] = names[i].take().or(name.clone());
        }
    }
    assert_eq!(
        ids,
        [Some("toolu_01".to_owned()), Some("toolu_02".to_owned())]
    );
    assert_eq!(names, [Some("ls".to_owned()), Some("pwd".to_owned())]);
    assert_eq!(
        serde_json::from_str::<Value>(&args[0]).unwrap(),
        json!({"path": "src"})
    );
    assert_eq!(serde_json::from_str::<Value>(&args[1]).unwrap(), json!({}));
    assert!(evs.contains(&StreamEvent::ReasoningDelta {
        text: "need files".into()
    }));
    assert_eq!(
        evs.last(),
        Some(&StreamEvent::Finish(FinishReason::ToolCalls))
    );
    let first = seen.recv().await.unwrap();
    assert!(first.contains("\"budget_tokens\":2048"), "{first}");

    // Follow-up tool turn: the signed block comes back from the adapter's
    // store even though Message carries no signature.
    let mut asst = user("Looking.");
    asst.role = Role::Assistant;
    asst.reasoning = Some("need files".into());
    asst.tool_calls = vec![ToolCall {
        id: "toolu_01".into(),
        name: "ls".into(),
        arguments: json!({"path": "src"}),
    }];
    let mut tool = user("main.rs");
    tool.role = Role::Tool;
    tool.tool_call_id = Some("toolu_01".into());
    let mut r2 = ChatRequest::new("claude-sonnet-4-5", vec![user("hi"), asst, tool]);
    r2.reasoning_effort = Some(ReasoningEffort::Low);
    events(&p, r2).await;
    let second = seen.recv().await.unwrap();
    assert!(second.contains("\"signature\":\"SIG-TURN-1\""), "{second}");
    assert!(second.contains("\"budget_tokens\":2048"));
    assert!(second.contains("\"tool_use_id\":\"toolu_01\""));
}

#[tokio::test]
async fn anthropic_cache_usage_mapped() {
    let (base, _rx) = mock_server(vec![ok("anthropic_cache.jsonl")]).await;
    let evs = events(&adapter(base, 0), req()).await;
    assert_eq!(
        evs,
        vec![
            usage(4, 1, 3000, 1200),
            StreamEvent::TextDelta { text: "ok".into() },
            usage(4, 9, 3000, 1200),
            StreamEvent::Finish(FinishReason::Length),
        ]
    );
    let StreamEvent::Usage(u) = &evs[2] else {
        panic!()
    };
    assert_eq!(u.total_input(), 4204);
}

#[tokio::test]
async fn anthropic_midstream_error_event_is_retryable_http() {
    let (base, _rx) = mock_server(vec![ok("anthropic_error_midstream.jsonl")]).await;
    let evs = run(&adapter(base, 0), req()).await;
    assert!(matches!(
        evs.get(1),
        Some(Ok(StreamEvent::TextDelta { text })) if text == "par"
    ));
    let Some(Err(e)) = evs.last() else {
        panic!("{evs:?}")
    };
    assert!(matches!(e, LlmError::Http { status: 529, .. }) && e.is_retryable());
}

#[tokio::test]
async fn anthropic_truncated_stream_is_protocol_error() {
    let (base, _rx) = mock_server(vec![ok("anthropic_truncated.jsonl")]).await;
    let evs = run(&adapter(base, 0), req()).await;
    assert!(matches!(evs.last(), Some(Err(LlmError::Protocol(_)))));
}

#[tokio::test]
async fn anthropic_http_auth_error_scrubbed_and_not_retried() {
    let body = fixture("anthropic_error_http.jsonl");
    let (base, _rx) = mock_server(vec![http(
        "401 Unauthorized",
        "application/json",
        body.trim(),
    )])
    .await;
    let p = adapter(base, 3);
    let e = p
        .stream(req(), CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert!(matches!(e, LlmError::Auth(_)), "{e}");
    let shown = format!("{e} {p:?}");
    assert!(!shown.contains("sk-ant-secret"), "{shown}");
}

#[tokio::test]
async fn anthropic_http_errors_overload_and_context() {
    let overloaded = http(
        "529 Overloaded",
        "application/json",
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
    );
    let (base, _rx) = mock_server(vec![overloaded.clone(), ok("anthropic_text.jsonl")]).await;
    let evs = run(&adapter(base, 1), req()).await;
    assert!(matches!(
        evs.last(),
        Some(Ok(StreamEvent::Finish(FinishReason::Stop)))
    ));

    let long = http(
        "400 Bad Request",
        "application/json",
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#,
    );
    let (base, _rx) = mock_server(vec![long]).await;
    let e = adapter(base, 0)
        .stream(req(), CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert!(matches!(e, LlmError::ContextLength(_)), "{e}");
}

#[tokio::test]
async fn anthropic_models_listing() {
    let body = http(
        "200 OK",
        "application/json",
        r#"{"data":[{"id":"claude-sonnet-4-5","display_name":"Claude Sonnet 4.5","max_input_tokens":200000,"max_tokens":64000,"capabilities":{"thinking":{"supported":true},"image_input":{"supported":true}}},{"id":"claude-x"}],"has_more":false}"#,
    );
    let (base, mut seen) = mock_server(vec![body]).await;
    let m = adapter(base, 0).models().await.unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].name, "Claude Sonnet 4.5");
    assert_eq!(m[0].context_window, 200_000);
    assert_eq!(m[0].max_output, Some(64_000));
    assert!(m[0].capabilities.reasoning && m[0].capabilities.vision);
    assert!(!m[1].capabilities.reasoning);
    let raw = seen.recv().await.unwrap().to_ascii_lowercase();
    assert!(raw.starts_with("get /v1/models?limit=1000 "), "{raw}");
}
