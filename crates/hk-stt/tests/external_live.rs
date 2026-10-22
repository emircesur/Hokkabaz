//! ExternalWhisper against the real `hk-whisper` helper and a real model. Skipped unless
//! `HK_WHISPER_BIN`, `HK_WHISPER_MODEL` (ggml file) and `HK_WHISPER_AUDIO` (any WAV with English
//! speech) are set: the model is too big for the default test run.
#![allow(clippy::print_stderr)]

use std::path::PathBuf;

use hk_stt::external::{ExternalOptions, ExternalWhisper};
use hk_stt::{TranscribeOpts, Transcriber};

#[tokio::test]
async fn transcribes_with_the_real_helper() {
    let (Some(bin), Some(model), Some(audio)) = (
        std::env::var_os("HK_WHISPER_BIN"),
        std::env::var_os("HK_WHISPER_MODEL"),
        std::env::var_os("HK_WHISPER_AUDIO"),
    ) else {
        eprintln!("HK_WHISPER_{{BIN,MODEL,AUDIO}} not set; skipping");
        return;
    };
    let t = ExternalWhisper::new(ExternalOptions {
        helper: PathBuf::from(bin),
        model_path: PathBuf::from(model),
        threads: Some(4),
        default_language: Some("en".into()),
    })
    .unwrap();
    let tr = t
        .transcribe(&PathBuf::from(audio), &TranscribeOpts::default())
        .await
        .unwrap();
    assert!(tr.words.len() > 10, "{} words", tr.words.len());
    let text: String = tr
        .words
        .iter()
        .map(|w| w.text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("americans"), "{text}");
    // Word times are monotonic and inside the clip.
    assert!(tr.words.windows(2).all(|p| p[0].start <= p[1].start));
    assert!(tr.words.last().unwrap().end <= tr.duration + 0.5);
}
