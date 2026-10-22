//! StudioService end to end with a fake STT / LLM and real ffmpeg.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hk_media::Tools;
use hk_stt::{AudioSplitter, TranscribeOpts, Transcriber, Transcript, Word};
use hk_studio::edit::EditOp;
use hk_studio::highlights::HighlightOpts;
use hk_studio::llm::{Llm, ScriptedLlm};
use hk_studio::social::{Platform, SocialOpts};
use hk_studio::{Backends, RenderOpts, StudioEvent, StudioService};

#[derive(Debug)]
struct FakeStt;

#[async_trait]
impl Transcriber for FakeStt {
    async fn transcribe(&self, audio: &Path, _o: &TranscribeOpts) -> hk_stt::Result<Transcript> {
        assert!(audio.is_file(), "audio was extracted first");
        // 40 words, 0.5 s each, a sentence every 10 words.
        let words = (0..40)
            .map(|i| Word::new(if i % 10 == 9 { format!("w{i}.") } else { format!("w{i}") }, i as f64 * 0.5, i as f64 * 0.5 + 0.4))
            .collect();
        Ok(Transcript { language: Some("en".into()), duration: 20.0, words, segments: vec![] })
    }
}

#[derive(Debug)]
struct Fake {
    llm: Arc<ScriptedLlm>,
}

#[async_trait]
impl Backends for Fake {
    async fn llm(&self) -> hk_studio::Result<Arc<dyn Llm>> {
        Ok(self.llm.clone())
    }

    async fn transcriber(&self, _s: Arc<dyn AudioSplitter>) -> hk_studio::Result<(Arc<dyn Transcriber>, TranscribeOpts)> {
        Ok((Arc::new(FakeStt), TranscribeOpts::default()))
    }
}

#[tokio::test]
async fn import_transcribe_analyze_render_export() {
    if !Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("hk_svc_{}", hk_studio::project::new_id("t")));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    assert!(Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=1280x720:rate=25:duration=20", "-f", "lavfi", "-i", "sine=frequency=300:duration=20", "-shortest", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .arg(&src)
        .status()
        .unwrap()
        .success());

    let detect = r#"{"clips":[{"start":0,"end":10,"title":"Opening","hook":"Hello","reason":"strong start","kind":"insight","rank":80}]}"#;
    let score = r#"{"hook":70,"emotion":60,"novelty":50,"clarity":90,"pacing":60,"shareability":65,"platform_fit":80,"summary":"decent","strengths":[],"improvements":["tighter"]}"#;
    let social = r#"{"posts":{"tiktok":{"title":"Watch this","description":"A clip","hashtags":["fun","clip"]}}}"#;
    let llm = Arc::new(ScriptedLlm::new([detect, score, social]));
    let svc = StudioService::new(dir.join("projects"), Tools::from_env(), Arc::new(Fake { llm }));

    let live = svc.import(&src, Some("Demo")).await.unwrap();
    let id = live.snapshot().await.id;
    assert_eq!(svc.projects().await.unwrap()[0].name, "Demo");

    svc.transcribe(&id).await.unwrap();
    assert_eq!(live.snapshot().await.transcript.unwrap().words.len(), 40);

    let ids = svc.analyze(&id, &HighlightOpts { min_secs: 5.0, max_secs: 60.0, ..HighlightOpts::default() }, None, Some("shorts-karaoke")).await.unwrap();
    assert_eq!(ids.len(), 1);
    let clip = ids[0].clone();
    let snap = live.snapshot().await;
    let c = snap.clip(&clip).unwrap();
    assert_eq!(c.title, "Opening");
    assert!(c.score.as_ref().is_some_and(|s| s.total > 60));
    assert_eq!(c.template.as_deref(), Some("shorts-karaoke"));

    // Motion-based tracking on a synthetic test pattern works without a face model.
    let n = svc.track(&id, &clip, None).await.unwrap();
    assert!(n >= 10, "{n}");
    assert!(matches!(live.snapshot().await.clip(&clip).unwrap().reframe.as_ref().unwrap().mode, hk_studio::reframe::ReframeMode::Track));

    svc.social(&id, &clip, &[Platform::Tiktok], &SocialOpts::default()).await.unwrap();
    assert!(live.snapshot().await.clip(&clip).unwrap().social.is_some());

    live.edit(EditOp::DeleteRange { clip: clip.clone(), start: 2.0, end: 4.0 }, "agent").await.unwrap();

    let mut rx = live.subscribe();
    let job = svc.start_render(&id, &clip, RenderOpts::default()).await.unwrap();
    assert!(job.starts_with('r'));
    let done = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match rx.recv().await.unwrap() {
                StudioEvent::RenderDone { path, .. } => break Ok(path),
                StudioEvent::RenderFailed { error, .. } => break Err(error),
                _ => {}
            }
        }
    })
    .await
    .expect("render finished in time")
    .expect("render ok");
    let info = hk_media::probe(Path::new(&done)).await.unwrap();
    assert_eq!(info.display_size(), Some((1080, 1920)));
    assert!(info.duration.unwrap() < 8.5, "2 s cut out of 10: {:?}", info.duration);

    assert_eq!(svc.render_path(&id, &clip).await.unwrap().to_string_lossy(), done);
    let pkg = svc.export_package(&id, &clip, &dir.join("export")).await.unwrap();
    assert!(pkg.join("clip.mp4").is_file() && pkg.join("captions.srt").is_file() && pkg.join("tiktok.txt").is_file());

    // The project and transcript survive a restart of the service.
    let svc2 = StudioService::new(dir.join("projects"), Tools::from_env(), Arc::new(Fake { llm: Arc::new(ScriptedLlm::new(Vec::<String>::new())) }));
    let again = svc2.open(&id).await.unwrap().snapshot().await;
    assert_eq!(again.clips.len(), 1);
    assert!(again.transcript.is_some());
    std::fs::remove_dir_all(&dir).ok();
}
