//! Ollama native adapter against recorded fixtures. No live API.
//!
//! Fixture format: one NDJSON line per response chunk.

use std::time::Duration;

use futures::StreamExt;
use hk_llm::ollama::{Ollama, PullProgress};
use hk_llm::openai::RetryPolicy;
use hk_llm::{
    CancellationToken, ChatRequest, FinishReason, LlmError, Message, Provider, Role, StreamEvent,
    ToolSpec, Usage,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn fixture(name: &str) -> String {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(p).expect("fixture")
}

/// One-shot HTTP/1.1 server; returns base URL and the raw requests it saw.
async fn mock_server(
    responses: Vec<String>,
) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        for r in responses {
            let (mut sock, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 16384];
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

fn ndjson(status: &str, name: &str) -> String {
    http(status, "application/x-ndjson", &fixture(name))
}

fn adapter(base: String) -> Ollama {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    Ollama::with_client(client, base)
        .api_key("sk-secret")
        .retry(RetryPolicy {
            max_retries: 0,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
        })
}

fn req() -> ChatRequest {
    let mut r = ChatRequest::new(
        "qwen3:8b",
        vec![Message {
            images: Vec::new(),
            role: Role::User,
            content: "hi".into(),
            reasoning: None,
            tool_calls: vec![],
            tool_call_id: None,
        }],
    );
    r.tools = vec![ToolSpec {
        name: "read".into(),
        description: "read a file".into(),
        parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
    }];
    r
}

async fn collect(p: &Ollama) -> Vec<Result<StreamEvent, LlmError>> {
    p.stream(req(), CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await
}

#[tokio::test]
async fn ollama_lists_models_from_tags() {
    let body = http("200 OK", "application/json", &fixture("ollama_tags.json"));
    let (base, mut seen) = mock_server(vec![body]).await;
    let ms = adapter(base).models().await.unwrap();
    assert_eq!(
        ms.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["qwen3:8b", "llama3.1:8b"]
    );
    assert!(ms.iter().all(|m| m.provider == "ollama"));
    let raw = seen.recv().await.unwrap().to_ascii_lowercase();
    assert!(raw.starts_with("get /api/tags "), "{raw}");
}

#[tokio::test]
async fn ollama_chat_text_thinking_and_usage() {
    let (base, mut seen) = mock_server(vec![ndjson("200 OK", "ollama_text.jsonl")]).await;
    let evs: Vec<_> = collect(&adapter(base))
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        evs,
        vec![
            StreamEvent::ReasoningDelta { text: "hm".into() },
            StreamEvent::TextDelta { text: "Hel".into() },
            StreamEvent::TextDelta {
                text: "lo, dünya".into()
            },
            StreamEvent::Usage(Usage {
                input_tokens: 26,
                output_tokens: 9,
                ..Usage::default()
            }),
            StreamEvent::Finish(FinishReason::Stop),
        ]
    );
    let raw = seen.recv().await.unwrap();
    let lower = raw.to_ascii_lowercase();
    assert!(lower.starts_with("post /api/chat "), "{raw}");
    assert!(lower.contains("authorization: bearer sk-secret"));
    let body: Value = serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["stream"], true);
    assert_eq!(body["tools"][0]["function"]["name"], "read");
}

#[tokio::test]
async fn ollama_tool_calls_arrive_whole_and_finish_as_tool_calls() {
    let (base, _rx) = mock_server(vec![ndjson("200 OK", "ollama_tool_calls.jsonl")]).await;
    let evs: Vec<_> = collect(&adapter(base))
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        evs[0],
        StreamEvent::ToolCallDelta {
            index: 0,
            id: Some("ollama_call_0".into()),
            name: Some("read".into()),
            arguments_delta: r#"{"path":"a.rs"}"#.into(),
        }
    );
    assert_eq!(
        evs[1],
        StreamEvent::ToolCallDelta {
            index: 1,
            id: Some("call_xyz".into()),
            name: Some("glob".into()),
            arguments_delta: r#"{"p":"*"}"#.into(),
        }
    );
    assert_eq!(
        evs[2],
        StreamEvent::Usage(Usage {
            input_tokens: 120,
            output_tokens: 31,
            ..Usage::default()
        })
    );
    assert_eq!(
        evs.last(),
        Some(&StreamEvent::Finish(FinishReason::ToolCalls))
    );
}

#[tokio::test]
async fn ollama_midstream_error_line_surfaces_after_text() {
    let (base, _rx) = mock_server(vec![ndjson("200 OK", "ollama_error.jsonl")]).await;
    let mut it = collect(&adapter(base)).await.into_iter();
    assert_eq!(
        it.next().unwrap().unwrap(),
        StreamEvent::TextDelta { text: "par".into() }
    );
    let e = it.next().unwrap().unwrap_err();
    assert!(
        matches!(&e, LlmError::Protocol(m) if m.contains("llama runner process has terminated")),
        "{e}"
    );
    assert!(it.next().is_none());
}

#[tokio::test]
async fn ollama_truncated_stream_is_protocol_error() {
    // Fixture minus its terminal `done` line.
    let body: String = fixture("ollama_text.jsonl")
        .lines()
        .take(2)
        .map(|l| format!("{l}\n"))
        .collect();
    let (base, _rx) = mock_server(vec![http("200 OK", "application/x-ndjson", &body)]).await;
    let last = collect(&adapter(base)).await.pop().unwrap();
    assert!(matches!(last, Err(LlmError::Protocol(_))), "{last:?}");
}

#[tokio::test]
async fn ollama_http_404_model_not_found_is_scrubbed_error() {
    let body = http(
        "404 Not Found",
        "application/json",
        r#"{"error":"model 'nope' not found, key sk-secret"}"#,
    );
    let (base, _rx) = mock_server(vec![body]).await;
    let e = match adapter(base).stream(req(), CancellationToken::new()).await {
        Err(e) => e,
        Ok(_) => panic!("expected error"),
    };
    match e {
        LlmError::Http {
            status, message, ..
        } => {
            assert_eq!(status, 404);
            assert!(message.contains("model 'nope' not found"));
            assert!(!message.contains("sk-secret"), "{message}");
        }
        other => panic!("{other}"),
    }
}

#[tokio::test]
async fn ollama_pull_reports_progress_until_success() {
    let (base, mut seen) = mock_server(vec![ndjson("200 OK", "ollama_pull.jsonl")]).await;
    let items: Vec<PullProgress> = adapter(base)
        .pull("qwen3:8b", CancellationToken::new())
        .await
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
        .await;
    assert_eq!(items.len(), 6);
    assert_eq!(items[0].status, "pulling manifest");
    assert_eq!(items[1].fraction(), Some(0.25));
    assert_eq!(items[2].fraction(), Some(1.0));
    assert!(items.last().unwrap().is_success());
    let raw = seen.recv().await.unwrap();
    assert!(
        raw.to_ascii_lowercase().starts_with("post /api/pull "),
        "{raw}"
    );
    assert!(raw.contains(r#""model":"qwen3:8b""#));
}

#[tokio::test]
async fn ollama_pull_error_line_fails_stream() {
    let (base, _rx) = mock_server(vec![ndjson("200 OK", "ollama_pull_error.jsonl")]).await;
    let items: Vec<_> = adapter(base)
        .pull("nope", CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(items.len(), 2);
    assert!(items[0].is_ok());
    assert!(
        matches!(&items[1], Err(LlmError::Protocol(m)) if m.contains("file does not exist")),
        "{:?}",
        items[1]
    );
}
