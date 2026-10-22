//! Integration tests against a real ffmpeg. Skipped (with a note) when ffmpeg/ffprobe/libx264 are missing.
#![cfg(not(target_os = "android"))]
#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use hk_media::{
    AudioSpec, CropKey, CropPath, Ducking, Interp, MediaError, MusicMix, Overlay, OverlayKind,
    RenderSpec, Segment, Source, Tools,
};
use tempfile::TempDir;

fn tools_ok(t: &Tools) -> bool {
    let ok = |bin: &Path, arg: &str| {
        Command::new(bin)
            .arg(arg)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !ok(&t.ffmpeg, "-version") || !ok(&t.ffprobe, "-version") {
        return false;
    }
    Command::new(&t.ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("libx264"))
        .unwrap_or(false)
}

fn ff(t: &Tools, args: &[&str]) {
    let out = Command::new(&t.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .expect("spawn ffmpeg");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 6 s 320x240 testsrc + 440 Hz sine, muted between 2.0 s and 3.5 s.
fn fixture() -> Option<(Tools, TempDir, PathBuf)> {
    let t = Tools::from_env();
    if !tools_ok(&t) {
        eprintln!("skip: ffmpeg/ffprobe/libx264 not available");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.mp4");
    ff(
        &t,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=25:duration=6",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=6",
            "-af",
            "volume=enable='between(t,2,3.5)':volume=0",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
            src.to_str().unwrap(),
        ],
    );
    Some((t, dir, src))
}

const ASS: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 180\nPlayResY: 320\n\n\
[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
Style: Default,Sans,24,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,20,1\n\n\
[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
Dialogue: 0,0:00:00.50,0:00:02.00,Default,,0,0,0,,Hello\n";

fn seg(start: f64, end: f64) -> Segment {
    Segment {
        source: 0,
        start,
        end,
    }
}

#[tokio::test]
async fn missing_binary_is_reported() {
    let t = Tools::new("/nonexistent/hk-ffprobe", "/nonexistent/hk-ffprobe");
    let err = t.probe("x.mp4").await.unwrap_err();
    assert!(matches!(err, MediaError::BinaryNotFound(_)), "{err}");
}

#[tokio::test]
async fn probe_reports_streams() {
    let Some((t, _d, src)) = fixture() else {
        return;
    };
    let m = t.probe(&src).await.unwrap();
    assert!((m.duration.unwrap() - 6.0).abs() < 0.2, "{:?}", m.duration);
    let v = m.video().unwrap();
    assert_eq!((v.width, v.height), (Some(320), Some(240)));
    assert!((v.fps.unwrap() - 25.0).abs() < 0.01);
    assert_eq!(v.rotation, 0);
    let a = m.audio().unwrap();
    assert_eq!((a.channels, a.sample_rate), (Some(1), Some(44100)));
}

#[tokio::test]
async fn silencedetect_finds_gap() {
    let Some((t, _d, src)) = fixture() else {
        return;
    };
    let r = t.silencedetect(&src, -40.0, 0.5).await.unwrap();
    assert_eq!(r.len(), 1, "{r:?}");
    assert!(
        (r[0].start - 2.0).abs() < 0.15 && (r[0].end - 3.5).abs() < 0.15,
        "{r:?}"
    );
}

#[tokio::test]
async fn extract_and_split_audio() {
    let Some((t, d, src)) = fixture() else { return };
    let wav = d.path().join("a.wav");
    t.extract_audio(&src, &wav, AudioSpec::default())
        .await
        .unwrap();
    let a = t.probe(&wav).await.unwrap();
    let au = a.audio().unwrap();
    assert_eq!((au.channels, au.sample_rate), (Some(1), Some(16000)));
    let total = a.duration.unwrap();
    let chunks = t
        .split_audio(
            &wav,
            &d.path().join("chunks"),
            AudioSpec::default(),
            total,
            2.5,
            0.5,
        )
        .await
        .unwrap();
    let offsets: Vec<f64> = chunks.iter().map(|c| c.offset).collect();
    assert_eq!(offsets, vec![0.0, 2.0, 4.0]);
    for c in &chunks {
        let m = t.probe(&c.path).await.unwrap();
        assert!(
            (m.duration.unwrap() - c.duration).abs() < 0.1,
            "{c:?} vs {:?}",
            m.duration
        );
    }
}

#[tokio::test]
async fn render_cut_crop_and_burn_in() {
    let Some((t, d, src)) = fixture() else { return };
    let ass = d.path().join("cap tions.ass");
    std::fs::write(&ass, ASS).unwrap();
    let out = d.path().join("out.mp4");
    let plain_out = d.path().join("plain.mp4");
    let mut spec = RenderSpec::new(
        vec![Source::new(&src)],
        vec![seg(0.0, 2.0), seg(3.5, 6.0)],
        &out,
    );
    spec.audio_crossfade = Some(0.03);
    spec.size = Some((180, 320));
    spec.fps = Some(25.0);
    spec.subtitles = Some(ass);
    spec.preset = Some("ultrafast".into());
    spec.crop = Some(CropPath {
        keys: vec![
            CropKey {
                t: 0.0,
                x: 0.0,
                y: 0.0,
                w: 160.0,
                h: 240.0,
            },
            CropKey {
                t: 4.5,
                x: 160.0,
                y: 0.0,
                w: 160.0,
                h: 240.0,
            },
        ],
        interp: Interp::Smooth,
    });
    let mut plain = spec.clone();
    plain.subtitles = None;
    plain.output = plain_out.clone();
    t.render(plain).await.unwrap().wait().await.unwrap();
    let h = t.render(spec).await.unwrap();
    let rx = h.progress();
    let report = h.wait().await.unwrap();
    assert_eq!(report.output, out);
    assert!(rx.borrow().finished);
    assert_eq!(rx.borrow().fraction, 1.0);

    let m = t.probe(&out).await.unwrap();
    assert!((m.duration.unwrap() - 4.5).abs() < 0.15, "{:?}", m.duration);
    assert_eq!(m.display_size(), Some((180, 320)));
    assert!(m.has_audio());
    // Caption (0.5s..2s) must change the frame at 1s.
    assert_ne!(
        frame_md5(&t, &out, 1.0),
        frame_md5(&t, &plain_out, 1.0),
        "subtitles not burned in"
    );
}

fn frame_md5(t: &Tools, f: &Path, at: f64) -> String {
    let o = Command::new(&t.ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &at.to_string(),
            "-i",
        ])
        .arg(f)
        .args(["-frames:v", "1", "-f", "md5", "-"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[tokio::test]
async fn render_segmented_crop_overlays_and_music() {
    let Some((t, d, src)) = fixture() else { return };
    let logo = d.path().join("logo.png");
    ff(
        &t,
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=64x64",
            "-frames:v",
            "1",
            logo.to_str().unwrap(),
        ],
    );
    let broll = d.path().join("broll.mp4");
    ff(
        &t,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=25:duration=2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            broll.to_str().unwrap(),
        ],
    );
    let bed = d.path().join("bed.wav");
    ff(
        &t,
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=220:duration=1",
            bed.to_str().unwrap(),
        ],
    );

    let out = d.path().join("out2.mp4");
    let mut spec = RenderSpec::new(
        vec![Source::new(&src)],
        vec![seg(0.0, 2.0), seg(3.5, 6.0)],
        &out,
    );
    spec.size = Some((180, 320));
    spec.preset = Some("ultrafast".into());
    // Window grows over time: forces the segmented-crop path.
    spec.crop = Some(CropPath {
        keys: vec![
            CropKey {
                t: 0.0,
                x: 0.0,
                y: 0.0,
                w: 120.0,
                h: 213.0,
            },
            CropKey {
                t: 2.0,
                x: 40.0,
                y: 0.0,
                w: 160.0,
                h: 240.0,
            },
            CropKey {
                t: 4.5,
                x: 100.0,
                y: 0.0,
                w: 135.0,
                h: 240.0,
            },
        ],
        interp: Interp::Linear,
    });
    spec.overlays = vec![
        Overlay {
            path: logo,
            kind: OverlayKind::Image,
            start: 0.5,
            end: 3.0,
            x: Some("10".into()),
            y: Some("10".into()),
            width: Some(32),
            opacity: Some(0.7),
        },
        Overlay {
            path: broll,
            kind: OverlayKind::Video,
            start: 1.0,
            end: 3.0,
            x: None,
            y: None,
            width: Some(120),
            opacity: None,
        },
    ];
    spec.music = Some(MusicMix {
        path: bed,
        gain: 0.3,
        ducking: Some(Ducking::default()),
        loop_track: true,
    });
    let h = t.render(spec).await.unwrap();
    h.wait().await.unwrap();
    let m = t.probe(&out).await.unwrap();
    assert!((m.duration.unwrap() - 4.5).abs() < 0.2, "{:?}", m.duration);
    assert_eq!(m.display_size(), Some((180, 320)));
    assert!(m.has_audio());
}

#[tokio::test]
async fn render_failure_carries_stderr_tail() {
    let Some((t, d, src)) = fixture() else { return };
    let mut spec = RenderSpec::new(
        vec![Source::new(&src)],
        vec![seg(0.0, 1.0)],
        d.path().join("no_such_dir").join("o.mp4"),
    );
    spec.preset = Some("ultrafast".into());
    let err = t.render(spec).await.unwrap().wait().await.unwrap_err();
    match err {
        MediaError::Failed { stderr_tail, .. } => assert!(!stderr_tail.is_empty()),
        other => panic!("unexpected {other}"),
    }
}

#[tokio::test]
async fn render_cancel_kills_ffmpeg() {
    let Some((t, d, src)) = fixture() else { return };
    let out = d.path().join("slow.mp4");
    let mut spec = RenderSpec::new(vec![Source::new(&src)], vec![seg(0.0, 6.0)], &out);
    spec.size = Some((1920, 1080));
    spec.preset = Some("veryslow".into());
    let mut h = t.render(spec).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    h.cancel();
    let err = h.wait().await.unwrap_err();
    assert!(matches!(err, MediaError::Cancelled), "{err}");
}

#[tokio::test]
async fn thumbnail_and_proxy() {
    let Some((t, d, src)) = fixture() else { return };
    let jpg = d.path().join("t.jpg");
    t.thumbnail(&src, 1.0, &jpg, 160).await.unwrap();
    let m = t.probe(&jpg).await.unwrap();
    assert_eq!(m.display_size(), Some((160, 120)));

    let px = d.path().join("proxy.mp4");
    t.proxy(&src, &px, 120).await.unwrap();
    let m = t.probe(&px).await.unwrap();
    assert_eq!(m.display_size(), Some((160, 120)));
    assert!((m.duration.unwrap() - 6.0).abs() < 0.2);
}
