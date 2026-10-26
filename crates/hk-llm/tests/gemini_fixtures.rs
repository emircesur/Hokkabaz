//! Gemini adapter against recorded fixtures. No live API.
//!
//! Fixture format: one SSE `data:` payload per line (a `GenerateContentResponse`
//! each), replayed with CRLF framing as the real endpoint sends it.

use futures::StreamExt;
use hk_llm::gemini::Gemini;
use hk_llm::openai::RetryPolicy;
use hk_llm::{
    CancellationToken, ChatRequest, FinishReason, LlmError, Message, Provider, ReasoningEffort,
    Role, StreamEvent, ToolCall, Usage,
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const KEY: &str = "AIza-secret";

fn fixture(name: &str) -> String {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(p).expect("fixture")
}

fn sse(name: &str) -> String {
    fixture(name)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| format!("data: {l}\r\n\r\n"))
        .collect()
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

fn adapter(base: String, retries: u32) -> Gemini {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    Gemini::with_client(client, KEY)
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
    ChatRequest::new("gemini-2.5-flash", vec![user("hi")])
}

async fn run(p: &Gemini, r: ChatRequest) -> Vec<Result<StreamEvent, LlmError>> {
    p.stream(r, CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await
}

async fn events(p: &Gemini, r: ChatRequest) -> Vec<StreamEvent> {
    run(p, r).await.into_iter().map(|e| e.unwrap()).collect()
}

fn usage(i: u64, o: u64, cached: u64, reasoning: u64) -> StreamEvent {
    StreamEvent::Usage(Usage {
        input_tokens: i,
        output_tokens: o,
        cache_read_tokens: cached,
        cache_write_tokens: 0,
        reasoning_tokens: reasoning,
    })
}

#[tokio::test]
async fn gemini_text_stream_cache_usage_and_request_shape() {
    let (base, mut seen) = mock_server(vec![ok("gemini_text.jsonl")]).await;
    let p = adapter(base, 0);
    assert_eq!(p.id(), "gemini");
    let mut r = req();
    r.messages.insert(
        0,
        Message {
            role: Role::System,
            ..user("be brief")
        },
    );
    assert_eq!(
        events(&p, r).await,
        vec![
            StreamEvent::TextDelta {
                text: "Hello".into()
            },
            usage(25, 0, 0, 0),
            StreamEvent::TextDelta {
                text: ", dünya".into()
            },
            usage(25, 3, 0, 0),
            StreamEvent::TextDelta { text: "!".into() },
            // promptTokenCount includes the cached 10.
            usage(15, 4, 10, 0),
            StreamEvent::Finish(FinishReason::Stop),
        ]
    );
    let raw = seen.recv().await.unwrap();
    let lower = raw.to_ascii_lowercase();
    assert!(
        lower.starts_with("post /v1beta/models/gemini-2.5-flash:streamgeneratecontent?alt=sse "),
        "{raw}"
    );
    assert!(lower.contains("x-goog-api-key: aiza-secret"));
    assert!(!lower.contains("authorization") && !lower.contains("?key="));
    assert!(raw.contains("\"systemInstruction\"") && raw.contains("\"role\":\"user\""));
}

#[tokio::test]
async fn gemini_thinking_maps_to_reasoning_deltas() {
    let (base, mut seen) = mock_server(vec![ok("gemini_thinking.jsonl")]).await;
    let mut r = req();
    r.reasoning_effort = Some(ReasoningEffort::Medium);
    let evs = events(&adapter(base, 0), r).await;
    assert_eq!(
        evs,
        vec![
            StreamEvent::ReasoningDelta {
                text: "Let me ".into()
            },
            usage(30, 0, 0, 0),
            StreamEvent::ReasoningDelta {
                text: "think.".into()
            },
            StreamEvent::TextDelta { text: "42".into() },
            // thoughts count as output and are broken out as reasoning.
            usage(30, 41, 0, 40),
            StreamEvent::Finish(FinishReason::Stop),
        ]
    );
    let raw = seen.recv().await.unwrap();
    assert!(raw.contains("\"includeThoughts\":true") && raw.contains("\"thinkingBudget\":8192"));
}

#[tokio::test]
async fn gemini_tool_calls_synthesize_ids_and_signature_is_replayed() {
    let follow_up = ok("gemini_text.jsonl");
    let (base, mut seen) = mock_server(vec![ok("gemini_tool_call.jsonl"), follow_up]).await;
    let p = adapter(base, 0);
    let evs = events(&p, req()).await;

    let calls: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => Some((
                *index,
                id.clone().unwrap(),
                name.clone().unwrap(),
                arguments_delta.clone(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].1.starts_with("gemini_") && calls[0].1 != calls[1].1);
    assert_eq!((calls[0].0, calls[0].2.as_str()), (0, "ls"));
    assert_eq!((calls[1].0, calls[1].2.as_str()), (1, "pwd"));
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].3).unwrap(),
        json!({"path": "src"})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&calls[1].3).unwrap(),
        json!({})
    );
    // STOP + tool calls => ToolCalls.
    assert_eq!(
        evs.last(),
        Some(&StreamEvent::Finish(FinishReason::ToolCalls))
    );
    seen.recv().await.unwrap();

    // Follow-up: functionResponse labelled by name, signature restored.
    let mut asst = user("Looking.");
    asst.role = Role::Assistant;
    asst.tool_calls = calls
        .iter()
        .map(|(_, id, name, args)| ToolCall {
            id: id.clone(),
            name: name.clone(),
            arguments: serde_json::from_str(args).unwrap(),
        })
        .collect();
    let mut t1 = user("main.rs");
    t1.role = Role::Tool;
    t1.tool_call_id = Some(calls[0].1.clone());
    let mut t2 = user("/w");
    t2.role = Role::Tool;
    t2.tool_call_id = Some(calls[1].1.clone());
    events(
        &p,
        ChatRequest::new("gemini-2.5-flash", vec![user("hi"), asst, t1, t2]),
    )
    .await;
    let second = seen.recv().await.unwrap();
    let body: Value = serde_json::from_str(second.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let c = body["contents"].as_array().unwrap();
    assert_eq!(c.len(), 3, "{body}");
    assert_eq!(c[1]["role"], "model");
    assert_eq!(c[1]["parts"][1]["thoughtSignature"], "SIG-TURN-1");
    assert_eq!(c[2]["parts"][0]["functionResponse"]["name"], "ls");
    assert_eq!(c[2]["parts"][1]["functionResponse"]["name"], "pwd");
}

#[tokio::test]
async fn gemini_blocked_prompt_finishes_content_filter() {
    let (base, _rx) = mock_server(vec![ok("gemini_blocked.jsonl")]).await;
    assert_eq!(
        events(&adapter(base, 0), req()).await,
        vec![
            usage(7, 0, 0, 0),
            StreamEvent::Finish(FinishReason::ContentFilter)
        ]
    );
}

#[tokio::test]
async fn gemini_candidate_safety_stop_after_partial_text() {
    let (base, _rx) = mock_server(vec![ok("gemini_blocked_candidate.jsonl")]).await;
    let evs = events(&adapter(base, 0), req()).await;
    assert!(evs.contains(&StreamEvent::TextDelta {
        text: "Sure, first".into()
    }));
    assert_eq!(
        evs.last(),
        Some(&StreamEvent::Finish(FinishReason::ContentFilter))
    );
}

#[tokio::test]
async fn gemini_max_tokens_is_length() {
    let (base, _rx) = mock_server(vec![ok("gemini_max_tokens.jsonl")]).await;
    let evs = events(&adapter(base, 0), req()).await;
    assert_eq!(evs.last(), Some(&StreamEvent::Finish(FinishReason::Length)));
}

#[tokio::test]
async fn gemini_midstream_error_is_retryable_http() {
    let (base, _rx) = mock_server(vec![ok("gemini_error_midstream.jsonl")]).await;
    let evs = run(&adapter(base, 0), req()).await;
    assert!(matches!(
        evs.first(),
        Some(Ok(StreamEvent::TextDelta { text })) if text == "par"
    ));
    let Some(Err(e)) = evs.last() else {
        panic!("{evs:?}")
    };
    assert!(matches!(e, LlmError::Http { status: 503, .. }) && e.is_retryable());
}

#[tokio::test]
async fn gemini_truncated_stream_is_protocol_error() {
    let (base, _rx) = mock_server(vec![ok("gemini_truncated.jsonl")]).await;
    let evs = run(&adapter(base, 0), req()).await;
    assert!(matches!(evs.last(), Some(Err(LlmError::Protocol(_)))));
}

#[tokio::test]
async fn gemini_http_bad_key_is_auth_scrubbed_and_not_retried() {
    let body = fixture("gemini_error_http.jsonl");
    let (base, _rx) = mock_server(vec![http(
        "400 Bad Request",
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
    assert!(!shown.contains(KEY), "{shown}");
    assert!(shown.contains("<redacted>"));
}

#[tokio::test]
async fn gemini_http_errors_rate_limit_retried_and_context_overflow() {
    let limited = http(
        "429 Too Many Requests",
        "application/json",
        r#"{"error":{"code":429,"message":"Resource has been exhausted","status":"RESOURCE_EXHAUSTED"}}"#,
    );
    let (base, _rx) = mock_server(vec![limited, ok("gemini_text.jsonl")]).await;
    let evs = run(&adapter(base, 1), req()).await;
    assert!(matches!(
        evs.last(),
        Some(Ok(StreamEvent::Finish(FinishReason::Stop)))
    ));

    let long = http(
        "400 Bad Request",
        "application/json",
        r#"{"error":{"code":400,"message":"The input token count (1200000) exceeds the maximum number of tokens allowed (1048576).","status":"INVALID_ARGUMENT"}}"#,
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
async fn gemini_rejects_path_altering_model_id() {
    let p = adapter("http://127.0.0.1:1".into(), 0);
    let e = p
        .stream(
            ChatRequest::new("x/../../v1beta/models", vec![user("hi")]),
            CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(e, LlmError::InvalidRequest(_)));
}

#[tokio::test]
async fn gemini_models_listing() {
    let body = http(
        "200 OK",
        "application/json",
        r#"{"models":[{"name":"models/gemini-2.5-pro","displayName":"Gemini 2.5 Pro","inputTokenLimit":1048576,"outputTokenLimit":65536,"supportedGenerationMethods":["generateContent","countTokens"],"thinking":true},{"name":"models/text-embedding-004","supportedGenerationMethods":["embedContent"]}]}"#,
    );
    let (base, mut seen) = mock_server(vec![body]).await;
    let m = adapter(base, 0).models().await.unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].id, "gemini-2.5-pro");
    assert_eq!(m[0].context_window, 1_048_576);
    assert_eq!(m[0].max_output, Some(65_536));
    assert!(m[0].capabilities.reasoning && m[0].capabilities.tools);
    let raw = seen.recv().await.unwrap().to_ascii_lowercase();
    assert!(
        raw.starts_with("get /v1beta/models?pagesize=1000 "),
        "{raw}"
    );
    assert!(raw.contains("x-goog-api-key: aiza-secret"));
}
