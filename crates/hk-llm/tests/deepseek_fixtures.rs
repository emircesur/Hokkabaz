//! DeepSeek adapter against recorded fixtures. No live API.
//!
//! Fixture format: one SSE `data:` payload per line (`[DONE]` literal included).

use std::time::Duration;

use bytes::Bytes;
use futures::{StreamExt, stream};
use hk_llm::deepseek::{DeepSeek, RetryPolicy, decode_stream, error_from_response};
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

/// Re-frame as SSE, with a keep-alive comment and CRLF to exercise the decoder.
fn sse(name: &str) -> String {
    let mut out = String::from(": keep-alive\n\n");
    for l in fixture(name).lines().filter(|l| !l.is_empty()) {
        out.push_str(&format!("data: {l}\r\n\r\n"));
    }
    out
}

async fn run(body: &str, chunk: usize) -> Vec<Result<StreamEvent, LlmError>> {
    // Tiny chunks split lines and UTF-8 sequences mid-way.
    let chunks: Vec<Result<Bytes, std::io::Error>> = body
        .as_bytes()
        .chunks(chunk)
        .map(|c| Ok(Bytes::copy_from_slice(c)))
        .collect();
    decode_stream(
        stream::iter(chunks),
        CancellationToken::new(),
        Duration::from_secs(5),
    )
    .collect()
    .await
}

fn ok(v: Vec<Result<StreamEvent, LlmError>>) -> Vec<StreamEvent> {
    v.into_iter().map(|r| r.expect("no error")).collect()
}

#[tokio::test]
async fn text_with_cache_usage() {
    for chunk in [7, 4096] {
        let evs = ok(run(&sse("text.jsonl"), chunk).await);
        assert_eq!(
            evs,
            vec![
                StreamEvent::TextDelta { text: "Hel".into() },
                StreamEvent::TextDelta {
                    text: "lo, dünya".into()
                },
                StreamEvent::Usage(Usage {
                    input_tokens: 8,
                    output_tokens: 5,
                    cache_read_tokens: 12,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                }),
                StreamEvent::Finish(FinishReason::Stop),
            ]
        );
    }
}

#[tokio::test]
async fn reasoning_then_answer() {
    let evs = ok(run(&sse("reasoning.jsonl"), 13).await);
    assert_eq!(
        evs,
        vec![
            StreamEvent::ReasoningDelta {
                text: "Think".into()
            },
            StreamEvent::ReasoningDelta {
                text: "ing...".into()
            },
            StreamEvent::TextDelta { text: "42".into() },
            StreamEvent::Usage(Usage {
                input_tokens: 10,
                output_tokens: 30,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 25,
            }),
            StreamEvent::Finish(FinishReason::Length),
        ]
    );
}

#[tokio::test]
async fn tool_call_deltas_assemble() {
    let evs = ok(run(&sse("tool_calls.jsonl"), 9).await);
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
    assert_eq!(
        ids,
        [Some("call_0_a".to_owned()), Some("call_1_b".to_owned())]
    );
    assert_eq!(names, [Some("read".to_owned()), Some("glob".to_owned())]);
    assert_eq!(
        serde_json::from_str::<Value>(&args[0]).unwrap()["path"],
        "a.rs"
    );
    assert_eq!(serde_json::from_str::<Value>(&args[1]).unwrap()["p"], "*");
    assert!(matches!(
        evs[evs.len() - 2],
        StreamEvent::Usage(Usage {
            cache_read_tokens: 64,
            input_tokens: 36,
            ..
        })
    ));
    assert_eq!(
        evs.last(),
        Some(&StreamEvent::Finish(FinishReason::ToolCalls))
    );
}

#[test]
fn http_errors_map() {
    for line in fixture("error_http.jsonl").lines() {
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
            "http_retryable" => {
                assert!(
                    matches!(e, LlmError::Http { .. }) && e.is_retryable(),
                    "{case}: {e}"
                );
                if let LlmError::Http {
                    retry_after: ra, ..
                } = &e
                {
                    assert_eq!(*ra, retry_after, "{case}");
                }
            }
            "http_fatal" => assert!(
                matches!(e, LlmError::Http { .. }) && !e.is_retryable(),
                "{case}"
            ),
            "auth" => assert!(matches!(e, LlmError::Auth(_)), "{case}: {e}"),
            "invalid_request" => assert!(matches!(e, LlmError::InvalidRequest(_)), "{case}: {e}"),
            "context_length" => assert!(matches!(e, LlmError::ContextLength(_)), "{case}: {e}"),
            x => panic!("unknown expect {x}"),
        }
    }
}

#[tokio::test]
async fn midstream_error_after_partial_text() {
    let r = run(&sse("error_midstream.jsonl"), 64).await;
    assert_eq!(r.len(), 2);
    assert!(matches!(&r[0], Ok(StreamEvent::TextDelta { text }) if text == "par"));
    assert!(matches!(&r[1], Err(LlmError::Protocol(m)) if m.contains("upstream exploded")));
}

#[tokio::test]
async fn truncated_stream_is_protocol_error() {
    let r = run(&sse("truncated.jsonl"), 64).await;
    assert!(matches!(r.last(), Some(Err(LlmError::Protocol(_)))));
}

#[tokio::test]
async fn cancel_ends_with_cancelled() {
    let cancel = CancellationToken::new();
    let pending = stream::pending::<Result<Bytes, std::io::Error>>();
    let mut s = decode_stream(pending, cancel.clone(), Duration::from_secs(60));
    cancel.cancel();
    assert!(matches!(s.next().await, Some(Err(LlmError::Cancelled))));
    assert!(s.next().await.is_none());
}

/// One-shot HTTP/1.1 server returning canned responses in order.
async fn mock_server(responses: Vec<String>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        for r in responses {
            let (mut sock, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 16384];
            let _ = sock.read(&mut buf).await;
            sock.write_all(r.as_bytes()).await.unwrap();
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}")
}

fn http(status: &str, ct: &str, extra: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: {ct}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn adapter(base: String, retries: u32) -> DeepSeek {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    DeepSeek::with_client(client, "sk-secret")
        .base_url(base)
        .retry(RetryPolicy {
            max_retries: retries,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
        })
}

fn req() -> ChatRequest {
    ChatRequest::new(
        "deepseek-chat",
        vec![Message { images: Vec::new(),
            role: Role::User,
            content: "hi".into(),
            reasoning: None,
            tool_calls: vec![],
            tool_call_id: None,
        }],
    )
}

#[tokio::test]
async fn retries_503_then_streams() {
    let err = http(
        "503 Service Unavailable",
        "application/json",
        "",
        r#"{"error":{"message":"Server is busy"}}"#,
    );
    let okr = http("200 OK", "text/event-stream", "", &sse("text.jsonl"));
    let p = adapter(mock_server(vec![err, okr]).await, 2);
    let s = p.stream(req(), CancellationToken::new()).await.unwrap();
    let evs: Vec<_> = s.collect().await;
    assert_eq!(evs.len(), 4);
    assert!(matches!(
        evs.last(),
        Some(Ok(StreamEvent::Finish(FinishReason::Stop)))
    ));
}

#[tokio::test]
async fn auth_error_not_retried_and_key_not_leaked() {
    let err = http(
        "401 Unauthorized",
        "application/json",
        "",
        r#"{"error":{"message":"bad key sk-secret"}}"#,
    );
    let p = adapter(mock_server(vec![err]).await, 3);
    let e = p
        .stream(req(), CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert!(matches!(e, LlmError::Auth(_)));
    assert!(!format!("{e} {p:?}").contains("sk-secret"));
}

#[tokio::test]
async fn retries_exhausted_surfaces_http_error() {
    let mk = || {
        http(
            "429 Too Many Requests",
            "application/json",
            "retry-after: 0\r\n",
            r#"{"error":{"message":"slow down"}}"#,
        )
    };
    let p = adapter(mock_server(vec![mk(), mk()]).await, 1);
    let e = p
        .stream(req(), CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert!(matches!(e, LlmError::Http { status: 429, .. }));
}
