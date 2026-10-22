//! Fixture parsing + fake local HTTP server for the OpenAI-compatible transcriber.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hk_stt::{
    FillerSet, OpenAiCompatOptions, OpenAiCompatTranscriber, SttError, TranscribeOpts, Transcriber, parse_verbose_json,
};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn word_level_fixture() {
    let mut t = parse_verbose_json(&fixture("openai_words.json"), None).unwrap();
    assert_eq!(t.language.as_deref(), Some("en"));
    assert_eq!(t.words.len(), 7);
    assert_eq!(t.words[1].text, "world.");
    assert!(t.words.iter().all(|w| !w.flags.estimated));
    // zero-duration "is" repaired
    assert!(t.words[3].duration() >= hk_stt::MIN_WORD_DURATION - 1e-12);
    assert!(t.words[3].flags.retimed);
    // confidence borrowed from the segment avg_logprob
    assert!((t.words[0].confidence.unwrap() - (-0.1f64).exp() as f32).abs() < 1e-5);
    assert_eq!(t.segments.len(), 2);
    assert_eq!(t.segments[0].word_range, 0..2);
    assert_eq!(t.segments[1].word_range, 2..7);
    assert_eq!(t.segments[1].text, "This is um a test.");
    assert!((t.duration - 4.2).abs() < 1e-9);
    // downstream: fillers + gaps + cut list
    assert_eq!(FillerSet::for_language("en", false).mark(&mut t), 1);
    t.flag_gaps(0.5);
    assert!(t.words[1].flags.gap_after && t.words[2].flags.gap_before);
    let kept = t.kept_ranges(&t.filler_indices(), 0.0);
    assert_eq!(kept.len(), 2);
    assert!((kept[0].end - 2.3).abs() < 0.2); // run before "um" ends where um starts (midpoint rule off at 0 pad)
    assert!((kept[1].start - 2.5).abs() < 1e-9);
}

#[test]
fn segment_only_interpolates() {
    let mut t = parse_verbose_json(&fixture("segments_only.json"), None).unwrap();
    assert_eq!(t.language.as_deref(), Some("tr"));
    assert_eq!(t.words.len(), 6);
    assert!(t.words.iter().all(|w| w.flags.estimated));
    assert_eq!(t.words[0].text, "Merhaba");
    assert!((t.words[0].start - 0.0).abs() < 1e-9);
    assert!((t.words[1].end - 2.0).abs() < 1e-9);
    assert!(t.words[0].end < t.words[1].end);
    assert!((t.words[2].start - 3.0).abs() < 1e-9);
    assert!((t.words[5].end - 6.0).abs() < 1e-9);
    assert_eq!(t.segments.len(), 2);
    assert_eq!(t.segments[1].word_range, 2..6);
    assert_eq!(FillerSet::for_language("tr", false).mark(&mut t), 1); // "Şey"
    assert!(t.words[2].flags.filler);
}

#[test]
fn nested_and_plain_text_variants() {
    let t = parse_verbose_json(&fixture("segment_nested_words.json"), None).unwrap();
    assert_eq!(t.words.len(), 2);
    assert_eq!(t.words[0].text, "Hallo");
    assert_eq!(t.words[0].confidence, Some(0.98));
    assert!(!t.words[0].flags.estimated);
    assert_eq!(t.segments[0].word_range, 0..2);

    let t = parse_verbose_json(&fixture("text_only.json"), Some("en")).unwrap();
    assert_eq!(t.words.len(), 4);
    assert_eq!(t.segments.len(), 1);
    assert!((t.words[3].end - 3.0).abs() < 1e-9);
    assert_eq!(t.language.as_deref(), Some("en"));

    assert!(matches!(parse_verbose_json("nope", None), Err(SttError::Parse(_))));
    assert!(parse_verbose_json("{}", None).unwrap().words.is_empty());
}

/// Serves one canned raw response per connection, in order; records raw requests.
async fn serve(responses: Vec<String>) -> (String, Arc<Mutex<Vec<Vec<u8>>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        for resp in responses {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                buf.extend_from_slice(&chunk[..n]);
                if let Some(h) = find(&buf, b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..h]).to_ascii_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                        .unwrap_or(0);
                    if buf.len() >= h + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            seen2.lock().unwrap().push(buf);
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}/v1"), seen)
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn resp(status: &str, extra: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn fast_opts() -> OpenAiCompatOptions {
    OpenAiCompatOptions { backoff_base: Duration::from_millis(1), max_retries: 2, ..Default::default() }
}

fn audio_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("hk-stt-test-{}-{name}", std::process::id()));
    std::fs::write(&p, bytes).unwrap();
    p
}

#[tokio::test]
async fn multipart_request_and_retry_on_429() {
    let ok = fixture("openai_words.json");
    let (base, seen) = serve(vec![
        resp("429 Too Many Requests", "Retry-After: 0\r\n", r#"{"error":"slow down"}"#),
        resp("503 Service Unavailable", "", "busy"),
        resp("200 OK", "", &ok),
    ])
    .await;
    let path = audio_file("a.mp3", b"FAKE-AUDIO-BYTES");
    let tr = OpenAiCompatTranscriber::new(&format!("{base}/"), "whisper-1", Some("sk-test".into()), fast_opts()).unwrap();
    let opts = TranscribeOpts { language: Some("tr-TR".into()), prompt: Some("Hokkabaz, ffmpeg".into()), word_timestamps: true };
    let t = tr.transcribe(&path, &opts).await.unwrap();
    assert_eq!(t.words.len(), 7);

    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 3, "two retries then success");
    let all: Vec<String> = reqs.iter().map(|r| String::from_utf8_lossy(r).into_owned()).collect();
    for r in &all {
        assert!(r.starts_with("POST /v1/audio/transcriptions HTTP/1.1"), "{}", &r[..60]);
        assert!(r.to_ascii_lowercase().contains("authorization: bearer sk-test"));
        assert!(r.to_ascii_lowercase().contains("content-type: multipart/form-data; boundary="));
        for needle in [
            "name=\"model\"\r\n\r\nwhisper-1",
            "name=\"response_format\"\r\n\r\nverbose_json",
            "name=\"timestamp_granularities[]\"\r\n\r\nword",
            "name=\"timestamp_granularities[]\"\r\n\r\nsegment",
            "name=\"language\"\r\n\r\ntr",
            "name=\"prompt\"\r\n\r\nHokkabaz, ffmpeg",
            "name=\"file\"; filename=\"",
            "Content-Type: audio/mpeg\r\n\r\nFAKE-AUDIO-BYTES\r\n--",
        ] {
            assert!(r.contains(needle), "missing {needle:?}");
        }
    }
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn client_error_not_retried_and_key_not_leaked() {
    let (base, seen) = serve(vec![resp("401 Unauthorized", "", r#"{"error":"bad key"}"#)]).await;
    let path = audio_file("b.wav", b"x");
    let tr = OpenAiCompatTranscriber::new(&base, "m", Some("sk-secret".into()), fast_opts()).unwrap();
    let err = tr.transcribe(&path, &TranscribeOpts::default()).await.unwrap_err();
    assert!(matches!(err, SttError::Http { status: 401, .. }), "{err:?}");
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(!format!("{tr:?}").contains("sk-secret"));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn gives_up_after_retries_and_enforces_size_limit() {
    let (base, seen) = serve(vec![resp("500 Internal Server Error", "", "e"); 3]).await;
    let path = audio_file("c.ogg", b"0123456789");
    let tr = OpenAiCompatTranscriber::new(&base, "m", None, fast_opts()).unwrap();
    let err = tr.transcribe(&path, &TranscribeOpts::default()).await.unwrap_err();
    assert!(matches!(err, SttError::Http { status: 500, .. }), "{err:?}");
    assert_eq!(seen.lock().unwrap().len(), 3);

    let small = OpenAiCompatOptions { max_upload_bytes: 5, ..fast_opts() };
    let tr = OpenAiCompatTranscriber::new(&base, "m", None, small).unwrap();
    let err = tr.transcribe(&path, &TranscribeOpts::default()).await.unwrap_err();
    assert!(matches!(err, SttError::TooLarge { size: 10, limit: 5 }), "{err:?}");
    assert_eq!(tr.max_upload_bytes(), Some(5));
    let _ = std::fs::remove_file(path);
}
