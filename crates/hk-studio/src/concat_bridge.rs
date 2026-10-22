//! Studio clip <-> Concat project. "Edit in Concat" builds a Concat project from the clip's
//! current edit (kept ranges, captions, reframe path, B-roll) and opens the editor on it;
//! "Render with Concat" exports that project with Concat's engine and makes the result the clip's
//! rendered video. The hk-concat plan adapter does the lowering to Concat's commands.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hk_concat::plan::{
    CaptionStyle, CaptionTrack, CropKeyframe, Overlay, PlanFraming, StudioClipPlan, TimeRange, Word,
};
use hk_concat::{ConcatClient, ConcatConfig, ConcatMode, ExportSpec, ExportState, VideoSettings};

use crate::pipeline::PreviewPlan;
use crate::project::StudioProject;
use crate::{Result, StudioError};

fn cerr(e: impl std::fmt::Display) -> StudioError {
    StudioError::Invalid(format!("Concat: {e}"))
}

/// What the Studio decided for this clip, as a Concat plan.
pub fn plan_for_clip(p: &StudioProject, clip_id: &str, pv: &PreviewPlan) -> Result<StudioClipPlan> {
    let clip = p.clip(clip_id)?;
    let mut plan = StudioClipPlan::new(
        clip.id.clone(),
        p.source.path.to_string_lossy().into_owned(),
        pv.ranges
            .iter()
            .map(|&(start, end)| TimeRange { start, end })
            .collect(),
    );
    let words: Vec<Word> = pv
        .words
        .iter()
        .filter(|w| !w.deleted)
        .map(|w| Word {
            text: w.text.clone(),
            start: w.start,
            end: w.end,
        })
        .collect();
    if !words.is_empty() {
        plan.captions = Some(CaptionTrack {
            words,
            style: CaptionStyle::default(),
        });
    }
    let (ow, oh) = pv.out_size;
    plan.video = if oh > ow {
        VideoSettings::VERTICAL_1080
    } else if oh == ow {
        VideoSettings::SQUARE_1080
    } else {
        VideoSettings::LANDSCAPE_1080
    };
    if let Some(crop) = &pv.crop {
        let (sw, sh) = (
            f64::from(p.source.width.max(1)),
            f64::from(p.source.height.max(1)),
        );
        let aspect = f64::from(ow.max(1)) / f64::from(oh.max(1));
        let fill_w = sh * aspect;
        plan.crop_keyframes = crop
            .keys
            .iter()
            .map(|k| CropKeyframe {
                time: k.t,
                center_x: ((k.x + k.w / 2.0) / sw).clamp(0.0, 1.0),
                center_y: ((k.y + k.h / 2.0) / sh).clamp(0.0, 1.0),
                zoom: if k.w > 0.0 {
                    (fill_w / k.w).max(1.0)
                } else {
                    1.0
                },
            })
            .collect();
        plan.framing = PlanFraming::Fill;
    }
    for b in &pv.broll {
        if b.asset.is_file() && b.end > b.start {
            plan.overlays.push(Overlay::Media {
                file: b.asset.to_string_lossy().into_owned(),
                start: b.start,
                duration: b.end - b.start,
                scale: 1.0,
                offset_x: 0.0,
                offset_y: 0.0,
                opacity: 1.0,
            });
        }
    }
    Ok(plan)
}

/// Concat reads and writes projects only under these folders (headless mode).
fn roots(data_dir: &Path) -> Vec<PathBuf> {
    let mut r = vec![data_dir.to_path_buf()];
    if let Some(h) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        r.push(PathBuf::from(h));
    }
    r
}

fn config(binary: &Path, data_dir: &Path) -> ConcatConfig {
    ConcatConfig {
        path: Some(binary.to_path_buf()),
        mode: ConcatMode::Headless,
        roots: roots(data_dir),
        ..ConcatConfig::default()
    }
}

/// Create (or rebuild) the Concat project of a clip and return its folder.
pub async fn prepare(
    binary: &Path,
    data_dir: &Path,
    work_dir: &Path,
    p: &StudioProject,
    clip_id: &str,
    pv: &PreviewPlan,
    rebuild: bool,
) -> Result<PathBuf> {
    let location = work_dir.join("concat");
    let dir = location.join(clip_id);
    if dir.is_dir() && !rebuild {
        return Ok(dir);
    }
    let plan = plan_for_clip(p, clip_id, pv)?;
    tokio::fs::create_dir_all(&location).await?;
    if dir.is_dir() {
        tokio::fs::remove_dir_all(&dir).await?;
    }
    let client = ConcatClient::connect(&config(binary, data_dir))
        .await
        .map_err(cerr)?;
    let res = async {
        let loc = location.to_string_lossy().into_owned();
        let applied = hk_concat::plan::apply_plan(&client, &loc, &plan).await?;
        client.project_save(&applied.project, None).await?;
        client.project_close(&applied.project, false).await?;
        Ok::<_, hk_concat::ConcatError>(())
    }
    .await;
    client.shutdown().await;
    res.map_err(cerr)?;
    Ok(dir)
}

/// Start the Concat window on `project` (detached; the user edits there).
pub fn open_window(binary: &Path, project: &Path) -> Result<()> {
    std::process::Command::new(binary)
        .arg("--project")
        .arg(project)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| StudioError::Invalid(format!("cannot start Concat: {e}")))
}

/// Export the project with Concat's engine to `output`, reporting progress (0..1).
pub async fn export(
    binary: &Path,
    data_dir: &Path,
    project: &Path,
    output: &Path,
    mut progress: impl FnMut(f64) + Send,
) -> Result<()> {
    if let Some(parent) = output.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let client = ConcatClient::connect(&config(binary, data_dir))
        .await
        .map_err(cerr)?;
    let proj = project.to_string_lossy().into_owned();
    let res = async {
        client.project_open(&proj).await?;
        let job = client
            .export_start(
                &proj,
                &ExportSpec {
                    output: output.to_string_lossy().into_owned(),
                    crf: Some(20),
                    preset: Some("medium".into()),
                    width: None,
                    height: None,
                    rate_num: None,
                    rate_den: None,
                    codec: Some("h264".into()),
                    ten_bit: None,
                },
            )
            .await?;
        let st = client
            .export_wait(
                &job.job,
                Duration::from_millis(500),
                Duration::from_secs(3 * 3600),
                |s| {
                    progress(s.fraction.clamp(0.0, 1.0));
                },
            )
            .await?;
        let _ = client.project_close(&proj, false).await;
        Ok::<_, hk_concat::ConcatError>(st)
    }
    .await;
    client.shutdown().await;
    let st = res.map_err(|e| {
        let m = e.to_string();
        if m.contains("lock") || m.contains("open in") {
            StudioError::Invalid(
                "Concat has this project open in its window. Close the Concat window, then render again.".into(),
            )
        } else {
            cerr(m)
        }
    })?;
    match st.state {
        ExportState::Done => Ok(()),
        ExportState::Cancelled => Err(StudioError::Invalid("Concat export was cancelled".into())),
        _ => Err(StudioError::Invalid(format!(
            "Concat export failed{}",
            st.error.map(|e| format!(": {e}")).unwrap_or_default()
        ))),
    }
}
