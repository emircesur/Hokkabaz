//! Studio through the FFI functions: local core, fake STT HTTP server, scripted LLM, real ffmpeg.

use std::process::Command;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use hk_ffi::api::studio::*;
use hk_ffi::dto::CoreProfile;
use hk_ffi::handle::CoreHandle;
use hk_ffi::local::{CoreOptions, LocalCore};
use hk_llm::{CancellationToken, Capabilities, ChatRequest, EventStream, FinishReason, ModelInfo, Provider, StreamEvent};
use hk_store::MemorySecretStore;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Replies with canned texts in order (highlight JSON, then score JSON).
struct Scripted(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl Provider for Scripted {
    fn id(&self) -> &str {
        "fake"
    }

    async fn models(&self) -> hk_llm::Result<Vec<ModelInfo>> {
        Ok(vec![ModelInfo {
            id: "m".into(),
            provider: "fake".into(),
            name: "Fake".into(),
            context_window: 8192,
            max_output: None,
            capabilities: Capabilities { tools: true, reasoning: false, vision: false },
            cost: None,
        }])
    }

    async fn stream(&self, _req: ChatRequest, _c: CancellationToken) -> hk_llm::Result<EventStream> {
        let text = {
            let mut v = self.0.lock().unwrap();
            if v.is_empty() { String::new() } else { v.remove(0) }
        };
        let evs = vec![Ok(StreamEvent::TextDelta { text }), Ok(StreamEvent::Finish(FinishReason::Stop))];
        Ok(futures::stream::iter(evs).boxed())
    }
}

/// One-shot-per-connection HTTP server answering any POST with a verbose_json transcript.
async fn fake_stt() -> String {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else { return };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 1 << 16];
                let mut got = Vec::new();
                // Read headers, then the declared body.
                loop {
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    got.extend_from_slice(&buf[..n]);
                    if let Some(h) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&got[..h]).to_lowercase();
                        let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length: ")).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                        if got.len() >= h + 4 + len {
                            break;
                        }
                    }
                }
                let words: Vec<Value> = (0..30).map(|i| json!({"word": format!("w{i}"), "start": i as f64 * 0.5, "end": i as f64 * 0.5 + 0.4})).collect();
                let body = json!({"language":"en","duration":15.0,"text":"x","words":words,"segments":[]}).to_string();
                let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/v1")
}

#[tokio::test]
async fn studio_through_ffi() {
    if !Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let detect = r#"{"clips":[{"start":0,"end":12,"title":"Strong open","hook":"h","reason":"r","kind":"insight","rank":90}]}"#;
    let score = r#"{"hook":80,"emotion":70,"novelty":60,"clarity":90,"pacing":70,"shareability":75,"platform_fit":85,"summary":"good","strengths":[],"improvements":[]}"#;
    let core = LocalCore::open(
        dir.path().join("data"),
        CoreProfile::Desktop,
        CoreOptions {
            secrets: Some(Arc::new(MemorySecretStore::default())),
            extra_providers: vec![Arc::new(Scripted(Mutex::new(vec![detect.into(), score.into()])))],
            refresh_catalog: false,
        },
    )
    .await
    .unwrap();

    let tools: Value = serde_json::from_str(&studio_check_tools().await.unwrap()).unwrap();
    assert_eq!(tools["ffmpeg"], true);

    let base = fake_stt().await;
    studio_save_settings(json!({"llm_model":"fake/m","stt_base_url": base,"stt_model":"whisper-1"}).to_string()).await.unwrap();
    studio_set_key("stt".into(), Some("sk-test".into())).await.unwrap();
    let s: Value = serde_json::from_str(&studio_get_settings().await.unwrap()).unwrap();
    assert_eq!(s["has_stt_key"], true);
    assert!(!s.to_string().contains("sk-test"), "keys are never returned");
    assert!(studio_save_settings(json!({"llm_model":"nomodel"}).to_string()).await.is_err());

    let src = dir.path().join("v.mp4");
    assert!(Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=640x360:rate=25:duration=15", "-f", "lavfi", "-i", "sine=duration=15", "-shortest", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .arg(&src)
        .status()
        .unwrap()
        .success());
    let p: Value = serde_json::from_str(&studio_import(src.to_string_lossy().into_owned(), Some("FFI".into())).await.unwrap()).unwrap();
    let id = p["id"].as_str().unwrap().to_string();
    assert_eq!(p["has_transcript"], false);

    studio_transcribe(id.clone()).await.unwrap();
    let ids: Vec<String> = serde_json::from_str(&studio_analyze(id.clone(), 3, 5.0, 60.0, None, Some("shorts-karaoke".into())).await.unwrap()).unwrap();
    assert_eq!(ids.len(), 1);
    let clip = ids[0].clone();

    let rev = studio_edit(id.clone(), json!([{"op":"delete_words","clip": clip,"words":[2,3,4]}]).to_string(), "ui".into()).await.unwrap();
    assert!(rev >= 1);
    let h: Value = serde_json::from_str(&studio_history(id.clone()).await.unwrap()).unwrap();
    assert_eq!(h["can_undo"], true);
    let prev: Value = serde_json::from_str(&studio_preview(id.clone(), clip.clone()).await.unwrap()).unwrap();
    assert!(prev["duration"].as_f64().unwrap() < 12.0);
    assert_eq!(prev["out_size"], json!([1080, 1920]));
    assert!(studio_undo(id.clone()).await.unwrap());
    let prev2: Value = serde_json::from_str(&studio_preview(id.clone(), clip.clone()).await.unwrap()).unwrap();
    assert!(prev2["duration"].as_f64().unwrap() > prev["duration"].as_f64().unwrap());
    assert!(studio_edit(id.clone(), r#"[{"op":"bogus"}]"#.into(), "ui".into()).await.is_err());

    let job = studio_render(id.clone(), clip.clone()).await.unwrap();
    assert!(!job.is_empty());
    let mut path = None;
    for _ in 0..240 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        path = studio_render_path(id.clone(), clip.clone()).await.unwrap();
        if path.is_some() {
            break;
        }
    }
    assert!(path.is_some(), "render finished");

    let t: Value = serde_json::from_str(&studio_templates().await.unwrap()).unwrap();
    assert!(t["templates"].as_array().unwrap().len() >= 5);
    studio_delete(id).await.unwrap();
    assert_eq!(serde_json::from_str::<Vec<Value>>(&studio_projects().await.unwrap()).unwrap().len(), 0);
    core.shutdown().await;
}
