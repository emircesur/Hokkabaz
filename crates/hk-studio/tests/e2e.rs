//! Real ffmpeg: import → (fake) transcript → text edits → live preview → render.

use std::path::Path;
use std::process::Command;

use hk_media::Tools;
use hk_stt::{Transcript, Word};
use hk_studio::edit::EditOp;
use hk_studio::{LiveStudio, RenderOpts, Studio, StudioEvent};

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn make_video(path: &Path) {
    let st = Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=1280x720:rate=25:duration=8",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=8",
            "-shortest",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(st.success());
}

fn transcript() -> Transcript {
    // 16 words, 0.5s each, "um" fillers at 3 and 9.
    let words = (0..16)
        .map(|i| {
            let mut w = Word::new(
                if i == 3 || i == 9 {
                    "um".to_string()
                } else {
                    format!("word{i}")
                },
                i as f64 * 0.5,
                i as f64 * 0.5 + 0.4,
            );
            w.flags.filler = i == 3 || i == 9;
            w
        })
        .collect();
    Transcript {
        language: Some("en".into()),
        duration: 8.0,
        words,
        segments: vec![],
    }
}

#[tokio::test]
async fn edit_preview_render() {
    if !have_ffmpeg() {
        return;
    }
    let dir =
        std::env::temp_dir().join(format!("hk_studio_e2e_{}", hk_studio::project::new_id("t")));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.mp4");
    make_video(&src);

    let studio = Studio::new(dir.join("projects"), Tools::from_env());
    let mut p = studio.import(&src, Some("demo")).await.unwrap();
    assert_eq!((p.source.width, p.source.height), (1280, 720));
    p.transcript = Some(transcript());
    studio.store.save_transcript(&p).await.unwrap();
    studio.store.save(&p).await.unwrap();
    // Round-trips through disk with its transcript.
    let again = studio.store.load(&p.id).await.unwrap();
    assert_eq!(again.transcript.as_ref().unwrap().words.len(), 16);

    let live = LiveStudio::new(studio.store.clone(), p);
    let mut events = live.subscribe();
    live.edit(
        EditOp::AddClip {
            title: "intro".into(),
            start: 0.0,
            end: 8.0,
        },
        "ui",
    )
    .await
    .unwrap();
    let clip = live.snapshot().await.clips[0].id.clone();
    let full = live.preview(&clip).await.unwrap().duration;
    assert!(full > 7.9, "untouched clip is ~full length: {full}");

    live.edit(
        EditOp::DeleteWords {
            clip: clip.clone(),
            words: vec![4, 5, 6, 7],
        },
        "ui",
    )
    .await
    .unwrap();
    live.edit(
        EditOp::SetFillers {
            clip: clip.clone(),
            on: true,
        },
        "agent",
    )
    .await
    .unwrap();
    live.edit(
        EditOp::ApplyTemplate {
            clip: clip.clone(),
            template: "shorts-karaoke".into(),
        },
        "agent",
    )
    .await
    .unwrap();
    let plan = live.preview(&clip).await.unwrap();
    assert!(
        plan.duration < full - 2.0,
        "words cut: {} vs {full}",
        plan.duration
    );
    assert!(
        plan.words.iter().filter(|w| w.deleted).count() >= 6,
        "4 deleted + 2 fillers"
    );
    assert!(
        plan.words
            .iter()
            .all(|w| w.deleted == w.out_start.is_none())
    );
    assert_eq!(plan.out_size, (1080, 1920));
    assert!(plan.crop.is_some() && !plan.captions.is_empty());

    // Events were broadcast to subscribers.
    let mut seen_agent = false;
    while let Ok(ev) = events.try_recv() {
        if matches!(ev, StudioEvent::Changed { ref by, .. } if by == "agent") {
            seen_agent = true;
        }
    }
    assert!(seen_agent);

    // Undo the template and redo it.
    assert!(live.undo().await.unwrap());
    assert!(live.snapshot().await.clips[0].reframe.is_none());
    assert!(live.redo().await.unwrap());

    let snap = live.snapshot().await;
    let handle = studio
        .render(&snap, &clip, &RenderOpts::default())
        .await
        .unwrap();
    let report = handle.wait().await.unwrap();
    let info = hk_media::probe(&report.output).await.unwrap();
    assert_eq!(info.display_size(), Some((1080, 1920)));
    let d = info.duration.unwrap();
    assert!(
        (d - plan.duration).abs() < 0.4,
        "rendered {d}s vs plan {}s",
        plan.duration
    );
    assert!(info.has_audio());

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn motion_tracking_follows_a_moving_box() {
    if !have_ffmpeg() {
        return;
    }
    use hk_studio::reframe::FocusSource;
    let dir = std::env::temp_dir().join(format!(
        "hk_studio_motion_{}",
        hk_studio::project::new_id("t")
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("box.mp4");
    let st = Command::new("ffmpeg")
        .args([
            "-y",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=640x360:r=10:d=6",
            "-f",
            "lavfi",
            "-i",
            "color=c=white:s=60x60:r=10:d=6",
            "-filter_complex",
            "[0][1]overlay=x='t*90':y=140",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&src)
        .status()
        .unwrap();
    assert!(st.success());
    let pts = hk_studio::vision::MotionFocus::new(Tools::from_env())
        .detect(&src, 0.0, 6.0, 0.5)
        .await
        .unwrap();
    assert!(pts.len() >= 10, "{}", pts.len());
    let early = pts[2].x;
    let late = pts[pts.len() - 2].x;
    assert!(late > early + 0.3, "box moves right: {early} -> {late}");
    std::fs::remove_dir_all(&dir).ok();
}

/// Needs the SeetaFace model and a test video with faces; set `HK_FACE_MODEL` / `HK_FACE_VIDEO` to run.
#[cfg(feature = "vision")]
#[tokio::test]
async fn face_tracking_finds_faces() {
    use hk_studio::reframe::FocusSource;
    let (Ok(model), Ok(video)) = (
        std::env::var("HK_FACE_MODEL"),
        std::env::var("HK_FACE_VIDEO"),
    ) else {
        return;
    };
    let pts = hk_studio::vision::FaceFocus::new(Tools::from_env(), model)
        .detect(Path::new(&video), 0.0, 3.0, 1.0)
        .await
        .unwrap();
    assert!(!pts.is_empty());
    // A group photo: the chosen face is off-centre on at least one axis.
    assert!(
        pts.iter()
            .any(|p| (p.x - 0.5).abs() > 0.02 || (p.y - 0.5).abs() > 0.02),
        "{pts:?}"
    );
}
