//! Video Studio API. Structured values cross the boundary as JSON strings (projects, previews,
//! events): the shapes are rich and evolve with the editor, and Dart already parses JSON.
//! Needs the in-process core; the Studio runs where ffmpeg is.

use std::path::Path;

use hk_studio::edit::EditOp;
use hk_studio::highlights::HighlightOpts;
use hk_studio::social::{Platform, SocialOpts};
use hk_studio::RenderOpts;
use serde_json::json;

use crate::error::{HkError, HkErrorKind};
use crate::frb_generated::StreamSink;
use crate::studio::{self, KEY_PEXELS, KEY_PIXABAY, KEY_STT, KEY_YT_REFRESH, KEY_YT_SECRET, StudioSettings, ctx, put_secret, service, studio_err};

fn json_err(e: serde_json::Error) -> HkError {
    HkError::invalid(format!("bad JSON: {e}"))
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<String, HkError> {
    serde_json::to_string(v).map_err(|e| HkError::internal(e.to_string()))
}

async fn project_json(id: &str) -> Result<String, HkError> {
    let p = service()?.open(id).await.map_err(studio_err)?.snapshot().await;
    let mut v = serde_json::to_value(&p).map_err(|e| HkError::internal(e.to_string()))?;
    v["has_transcript"] = json!(p.transcript.is_some());
    to_json(&v)
}

/// Is ffmpeg usable? `{ffmpeg: bool, ffprobe: bool, version: string|null}`.
pub async fn studio_check_tools() -> Result<String, HkError> {
    let t = hk_media::Tools::from_env();
    let run = |bin: std::path::PathBuf| async move {
        #[cfg(target_os = "android")]
        {
            let _ = bin;
            None::<String>
        }
        #[cfg(not(target_os = "android"))]
        {
            tokio::process::Command::new(bin)
                .arg("-version")
                .output()
                .await
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or_default().to_owned())
        }
    };
    let (ff, fp) = (run(t.ffmpeg.clone()).await, run(t.ffprobe.clone()).await);
    to_json(&json!({"ffmpeg": ff.is_some(), "ffprobe": fp.is_some(), "version": ff}))
}

/// Studio preferences plus which keys are set.
pub async fn studio_get_settings() -> Result<String, HkError> {
    let c = ctx()?;
    let s = c.settings.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let has = |n: &str| matches!(c.secrets.get(n), Ok(Some(_)));
    to_json(&json!({
        "settings": s,
        "has_stt_key": has(KEY_STT), "has_pexels_key": has(KEY_PEXELS), "has_pixabay_key": has(KEY_PIXABAY),
        "has_youtube_secret": has(KEY_YT_SECRET), "has_youtube_refresh": has(KEY_YT_REFRESH),
    }))
}

/// Save preferences (`StudioSettings` JSON).
pub async fn studio_save_settings(settings_json: String) -> Result<(), HkError> {
    let c = ctx()?;
    let s: StudioSettings = serde_json::from_str(&settings_json).map_err(json_err)?;
    s.validate()?;
    c.store.set_setting(studio::SETTINGS_KEY, &to_json(&s)?).await?;
    *c.settings.write().unwrap_or_else(std::sync::PoisonError::into_inner) = s;
    Ok(())
}

/// `which`: `stt` | `pexels` | `pixabay` | `youtube_secret` | `youtube_refresh`. Empty / `None` deletes the key. Never returned.
pub async fn studio_set_key(which: String, key: Option<String>) -> Result<(), HkError> {
    let name = match which.as_str() {
        "stt" => KEY_STT,
        "pexels" => KEY_PEXELS,
        "pixabay" => KEY_PIXABAY,
        "youtube_secret" => KEY_YT_SECRET,
        "youtube_refresh" => KEY_YT_REFRESH,
        o => return Err(HkError::invalid(format!("unknown key `{o}`"))),
    };
    put_secret(ctx()?.secrets.as_ref(), name, key.as_deref())
}

/// Built-in templates and caption presets: `{templates: [...], caption_presets: [...]}`.
pub async fn studio_templates() -> Result<String, HkError> {
    to_json(&json!({
        "templates": hk_studio::templates::builtin_templates(),
        "caption_presets": ["classic", "karaoke", "pop", "minimal"],
        "platforms": Platform::ALL.map(Platform::key),
    }))
}

pub async fn studio_projects() -> Result<String, HkError> {
    to_json(&service()?.projects().await.map_err(studio_err)?)
}

/// Import a video file; returns the project JSON.
pub async fn studio_import(path: String, name: Option<String>) -> Result<String, HkError> {
    let live = service()?.import(Path::new(&path), name.as_deref()).await.map_err(studio_err)?;
    project_json(&live.snapshot().await.id).await
}

pub async fn studio_project(id: String) -> Result<String, HkError> {
    project_json(&id).await
}

pub async fn studio_delete(id: String) -> Result<(), HkError> {
    service()?.delete(&id).await.map_err(studio_err)
}

/// Transcribe the source (minutes for long videos). The editor stays usable meanwhile.
pub async fn studio_transcribe(id: String) -> Result<(), HkError> {
    service()?.transcribe(&id).await.map_err(studio_err)
}

/// Find highlights and add them as scored clips. Returns the new clip ids as a JSON array.
pub async fn studio_analyze(
    id: String,
    count: u32,
    min_secs: f64,
    max_secs: f64,
    brief: Option<String>,
    template: Option<String>,
) -> Result<String, HkError> {
    let opts = HighlightOpts {
        count: count.clamp(1, 30) as usize,
        min_secs,
        max_secs,
        brief: brief.filter(|b| !b.trim().is_empty()),
        ..HighlightOpts::default()
    };
    let ids = service()?.analyze(&id, &opts, None, template.as_deref()).await.map_err(studio_err)?;
    to_json(&ids)
}

/// Apply edit operations (`EditOp` JSON array). `by` = `ui` | `agent`. Returns the new revision.
pub async fn studio_edit(id: String, ops_json: String, by: String) -> Result<u64, HkError> {
    let ops: Vec<EditOp> = serde_json::from_str(&ops_json).map_err(json_err)?;
    let live = service()?.open(&id).await.map_err(studio_err)?;
    let mut rev = live.rev().await;
    for op in ops {
        rev = live.edit(op, &by).await.map_err(studio_err)?;
    }
    Ok(rev)
}

pub async fn studio_undo(id: String) -> Result<bool, HkError> {
    service()?.open(&id).await.map_err(studio_err)?.undo().await.map_err(studio_err)
}

pub async fn studio_redo(id: String) -> Result<bool, HkError> {
    service()?.open(&id).await.map_err(studio_err)?.redo().await.map_err(studio_err)
}

/// `{can_undo, can_redo, rev}`.
pub async fn studio_history(id: String) -> Result<String, HkError> {
    let live = service()?.open(&id).await.map_err(studio_err)?;
    to_json(&json!({"can_undo": live.can_undo().await, "can_redo": live.can_redo().await, "rev": live.rev().await}))
}

/// Player-ready plan of a clip's current edit: kept ranges, words, captions, crop path, B-roll.
pub async fn studio_preview(id: String, clip: String) -> Result<String, HkError> {
    let plan = service()?.open(&id).await.map_err(studio_err)?.preview(&clip).await.map_err(studio_err)?;
    to_json(&plan)
}

/// Follow the speaker / subject and crop to vertical (or `square`). Returns the number of points.
pub async fn studio_reframe(id: String, clip: String, square: bool) -> Result<u32, HkError> {
    let base = if square { hk_studio::reframe::ReframeSpec::square() } else { hk_studio::reframe::ReframeSpec::vertical() };
    let n = service()?.track(&id, &clip, Some(base)).await.map_err(studio_err)?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Download the face-tracking model once (otherwise motion tracking is used).
pub async fn studio_prepare_face_model() -> Result<bool, HkError> {
    #[cfg(feature = "vision")]
    {
        Ok(service()?.ensure_face_model().await.map_err(studio_err)?.is_some())
    }
    #[cfg(not(feature = "vision"))]
    {
        Ok(false)
    }
}

pub async fn studio_broll(id: String, clip: String, max: u32) -> Result<u32, HkError> {
    let opts = hk_studio::broll::BrollOpts { max: max.clamp(1, 10) as usize, ..Default::default() };
    let n = service()?.add_broll(&id, &clip, &opts).await.map_err(studio_err)?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Titles / descriptions / hashtags. `platforms_json`: `["tiktok", ...]` (empty = all).
pub async fn studio_social(id: String, clip: String, platforms_json: String, tone: Option<String>, language: Option<String>) -> Result<(), HkError> {
    let platforms: Vec<Platform> = if platforms_json.trim().is_empty() {
        Platform::ALL.to_vec()
    } else {
        serde_json::from_str(&platforms_json).map_err(json_err)?
    };
    let opts = SocialOpts { tone, language, call_to_action: None };
    service()?.social(&id, &clip, if platforms.is_empty() { &Platform::ALL } else { &platforms }, &opts).await.map_err(studio_err)
}

/// Start rendering in the background; progress / completion arrive on `studio_events`.
pub async fn studio_render(id: String, clip: String) -> Result<String, HkError> {
    service()?.start_render(&id, &clip, RenderOpts::default()).await.map_err(studio_err)
}

pub async fn studio_cancel_render(job: String) -> Result<bool, HkError> {
    Ok(service()?.cancel_render(&job).await)
}

/// Path of the rendered MP4, if the clip was rendered.
pub async fn studio_render_path(id: String, clip: String) -> Result<Option<String>, HkError> {
    Ok(service()?.render_path(&id, &clip).await.map(|p| p.to_string_lossy().into_owned()))
}

/// Upload the rendered clip to YouTube. `privacy`: `private` (default) | `unlisted` | `public`.
/// Returns the video URL.
pub async fn studio_publish_youtube(id: String, clip: String, privacy: String) -> Result<String, HkError> {
    let privacy = match privacy.as_str() {
        "public" => hk_studio::publish::Privacy::Public,
        "unlisted" => hk_studio::publish::Privacy::Unlisted,
        _ => hk_studio::publish::Privacy::Private,
    };
    Ok(service()?.publish_youtube(&id, &clip, privacy).await.map_err(studio_err)?.url)
}

/// Write clip.mp4 + captions + per-platform post text into `dir`. Returns the folder.
pub async fn studio_export(id: String, clip: String, dir: String) -> Result<String, HkError> {
    let p = service()?.export_package(&id, &clip, Path::new(&dir)).await.map_err(studio_err)?;
    Ok(p.to_string_lossy().into_owned())
}

/// JPEG frame of the source at `at` seconds.
pub async fn studio_thumbnail(id: String, at: f64) -> Result<String, HkError> {
    let svc = service()?;
    let p = svc.open(&id).await.map_err(studio_err)?.snapshot().await;
    Ok(svc.studio().thumbnail(&p, at).await.map_err(studio_err)?.to_string_lossy().into_owned())
}

/// Low-res proxy of the source for smooth scrubbing (made once; may take a while).
pub async fn studio_proxy(id: String) -> Result<String, HkError> {
    let svc = service()?;
    let p = svc.open(&id).await.map_err(studio_err)?.snapshot().await;
    Ok(svc.studio().proxy(&p).await.map_err(studio_err)?.to_string_lossy().into_owned())
}

/// Live change feed of one project as JSON `StudioEvent`s (edits by you or the agent, transcript
/// ready, clips added, render progress). A `{"kind":"resync"}` event means events were missed:
/// reload the project.
pub async fn studio_events(id: String, sink: StreamSink<String>) -> Result<(), HkError> {
    let live = service()?.open(&id).await.map_err(studio_err)?;
    let mut rx = live.subscribe();
    loop {
        let msg = match rx.recv().await {
            Ok(ev) => to_json(&ev)?,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => r#"{"kind":"resync"}"#.to_owned(),
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
        };
        sink.add(msg).map_err(|e| HkError::new(HkErrorKind::Closed, e.to_string()))?;
    }
}
