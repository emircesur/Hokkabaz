//! OpenAI-compatible adapter against recorded fixtures. No live API.
//!
//! Fixture format: one SSE `data:` payload per line (`[DONE]` literal included).

use std::time::Duration;

use futures::StreamExt;
use hk_llm::openai::{OpenAiCompat, RetryPolicy, error_from_response};
use hk_llm::{
    CancellationToken, ChatRequest, FinishReason, LlmError, Message, Provider, Role, StreamEvent,
    Usage,
};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn fixture(name: &str) -> String {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(p).expect("fixture")
}

fn sse(name: &str) -> String {
    let mut out = String::from(": keep-alive\n\n");
    for l in fixture(name).lines().filter(|l| !l.is_empty()) {
        out.push_str(&format!("data: {l}\n\n"));
    }
    out
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
    (format!("http://{addr}/v1"), rx)
}

fn http(status: &str, ct: &str, extra: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {ct}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn adapter(base: String, retries: u32) -> OpenAiCompat {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    OpenAiCompat::with_client(client, "openrouter", base)
        .api_key("sk-secret")
        .header("x-title", "hokkabaz-test")
        .unwrap()
        .retry(RetryPolicy {
            max_retries: retries,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
        })
}

fn req() -> ChatRequest {
    ChatRequest::new(
        "gpt-4o-mini",
        vec![Message {
            role: Role::User,
            content: "hi".into(),
            reasoning: None,
            tool_calls: vec![],
            tool_call_id: None,
        }],
    )
}

async fn collect(p: &OpenAiCompat) -> Vec<Result<StreamEvent, LlmError>> {
    p.stream(req(), CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await
}

#[tokio::test]
async fn text_reasoning_alias_and_cached_usage_with_headers() {
    let body = http("200 OK", "text/event-stream", "", &sse("openai_text.jsonl"));
    let (base, mut seen) = mock_server(vec![body]).await;
    let p = adapter(base, 0);
    assert_eq!(p.id(), "openrouter");
    let evs: Vec<_> = collect(&p).await.into_iter().map(|r| r.unwrap()).collect();
    assert_eq!(
        evs,
        vec![
            StreamEvent::ReasoningDelta { text: "hm".into() },
            StreamEvent::TextDelta { text: "Hel".into() },
            StreamEvent::TextDelta {
                text: "lo, dünya".into()
            },
            StreamEvent::Usage(Usage {
                input_tokens: 8,
                output_tokens: 5,
                cache_read_tokens: 12,
                cache_write_tokens: 0,
                reasoning_tokens: 2,
            }),
            StreamEvent::Finish(FinishReason::Stop),
        ]
    );
    let raw = seen.recv().await.unwrap().to_ascii_lowercase();
    assert!(raw.starts_with("post /v1/chat/completions "), "{raw}");
    assert!(raw.contains("authorization: bearer sk-secret"));
    assert!(raw.contains("x-title: hokkabaz-test"));
    assert!(raw.contains("\"stream\":true"));
}

#[tokio::test]
async fn tool_call_deltas_assemble() {
    let body = http(
        "200 OK",
        "text/event-stream",
        "",
        &sse("openai_tool_calls.jsonl"),
    );
    let (base, _rx) = mock_server(vec![body]).await;
    let evs: Vec<_> = collect(&adapter(base, 0))
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    let mut args = [String::new(), String::new()];
    let mut names = [None, None];
    let mut ids = [None, None];
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
            names[i] = names[i].take().or(name.clone());
            ids[i] = ids[i].take().or(id.clone());
        }
    }
    assert_eq!(ids, [Some("call_a".to_owned()), Some("call_b".to_owned())]);
    assert_eq!(names, [Some("read".to_owned()), Some("glob".to_owned())]);
    assert_eq!(
        serde_json::from_str::<Value>(&args[0]).unwrap()["path"],
        "a.rs"
    );
    assert_eq!(serde_json::from_str::<Value>(&args[1]).unwrap()["p"], "*");
    assert_eq!(
        evs.last(),
        Some(&StreamEvent::Finish(FinishReason::ToolCalls))
    );
}

#[test]
fn http_errors_map() {
    for line in fixture("openai_error_http.jsonl").lines() {
        let c: Value = serde_json::from_str(line).unwrap();
        let retry_after = c["retry_after"]
            .as_str()
            .map(|s| Duration::from_secs(s.parse().unwrap()));
        let e = error_from_response(
            c["status"].as_u64().unwrap() as u16,
            retry_after,
            &c["body"].to_string(),
        );
        let case = c["case"].as_str().unwrap();
        match c["expect"].as_str().unwrap() {
            "http_retryable" => assert!(
                matches!(e, LlmError::Http { .. }) && e.is_retryable(),
                "{case}: {e}"
            ),
            "auth" => assert!(matches!(e, LlmError::Auth(_)), "{case}: {e}"),
            "invalid_request" => assert!(matches!(e, LlmError::InvalidRequest(_)), "{case}: {e}"),
            "context_length" => assert!(matches!(e, LlmError::ContextLength(_)), "{case}: {e}"),
            x => panic!("unknown expect {x}"),
        }
    }
}

#[tokio::test]
async fn retries_429_then_streams() {
    let err = http(
        "429 Too Many Requests",
        "application/json",
        "retry-after: 0\r\n",
        r#"{"error":{"message":"slow down"}}"#,
    );
    let okr = http("200 OK", "text/event-stream", "", &sse("openai_text.jsonl"));
    let (base, _rx) = mock_server(vec![err, okr]).await;
    let evs = collect(&adapter(base, 2)).await;
    assert_eq!(evs.len(), 5);
    assert!(matches!(
        evs.last(),
        Some(Ok(StreamEvent::Finish(FinishReason::Stop)))
    ));
}

#[tokio::test]
async fn auth_error_not_retried_and_secrets_not_leaked() {
    let err = http(
        "401 Unauthorized",
        "application/json",
        "",
        r#"{"error":{"message":"bad key sk-secret / hokkabaz-test"}}"#,
    );
    let (base, _rx) = mock_server(vec![err]).await;
    let p = adapter(base, 3);
    let e = p
        .stream(req(), CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert!(matches!(e, LlmError::Auth(_)));
    let shown = format!("{e} {p:?}");
    assert!(!shown.contains("sk-secret") && !shown.contains("hokkabaz-test"));
}

#[tokio::test]
async fn keyless_local_server_sends_no_auth_and_lists_models() {
    let models = http(
        "200 OK",
        "application/json",
        "",
        r#"{"object":"list","data":[{"id":"llama3","object":"model"},{"id":"qwen3"}]}"#,
    );
    let (base, mut seen) = mock_server(vec![models]).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let p = OpenAiCompat::with_client(client, "ollama", format!("{base}/"));
    let m = p.models().await.unwrap();
    assert_eq!(
        m.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["llama3", "qwen3"]
    );
    assert!(m.iter().all(|m| m.provider == "ollama"));
    let raw = seen.recv().await.unwrap().to_ascii_lowercase();
    assert!(raw.starts_with("get /v1/models "), "{raw}");
    assert!(!raw.contains("authorization"));
}

#[test]
fn bad_header_rejected_without_echo() {
    let e = OpenAiCompat::with_client(reqwest::Client::new(), "x", "http://h")
        .header("x-key", "line\nbreak-secret")
        .err()
        .unwrap();
    assert!(matches!(e, LlmError::InvalidRequest(_)));
    assert!(!e.to_string().contains("break-secret"));
}
