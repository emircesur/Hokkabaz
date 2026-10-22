//! Real download of ffmpeg from the internet. Skipped unless `HK_DEPS_LIVE=1`.
#![allow(clippy::print_stderr)]

use std::time::Duration;

use hk_deps::{Deps, FFMPEG};

#[tokio::test]
async fn installs_ffmpeg_and_the_media_layer_finds_it() {
    if std::env::var_os("HK_DEPS_LIVE").is_none() {
        eprintln!("HK_DEPS_LIVE not set; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let deps = Deps::new(dir.path());
    deps.install(FFMPEG).unwrap();
    let mut last = String::new();
    for _ in 0..900 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let s = deps
            .status()
            .await
            .into_iter()
            .find(|s| s.id == FFMPEG)
            .unwrap();
        if let Some(p) = &s.phase {
            if *p != last {
                eprintln!("phase: {p}");
                last = p.clone();
            }
        }
        if let Some(e) = &s.error {
            panic!("install failed: {e}");
        }
        if !s.installing {
            break;
        }
    }
    let s = deps
        .status()
        .await
        .into_iter()
        .find(|s| s.id == FFMPEG)
        .unwrap();
    // PATH may already have ffmpeg on the test machine; the managed copy still has to exist
    assert!(
        dir.path().join("tools/ffmpeg/ffmpeg").is_file()
            || dir.path().join("tools/ffmpeg/ffmpeg.exe").is_file()
    );
    assert!(s.installed, "{s:?}");
    eprintln!("{s:?}");
    assert_eq!(s.source, Some("managed"), "managed copy must win over PATH");
}
