//! Video Studio API. Structured values cross the boundary as JSON strings (projects, previews,
//! events): the shapes are rich and evolve with the editor, and Dart already parses JSON.
//!
//! Every call goes to the *active core*: the Studio runs where ffmpeg is, which is this device on
//! desktop and the connected instance on a phone.

use futures::StreamExt;
use serde_json::{Value, json};

use crate::error::{HkError, HkErrorKind};
use crate::frb_generated::StreamSink;
use crate::handle::core;

async fn rpc(method: &str, params: Value) -> Result<Value, HkError> {
    core()?.studio_rpc(method, params).await
}

async fn rpc_json(method: &str, params: Value) -> Result<String, HkError> {
    serde_json::to_string(&rpc(method, params).await?).map_err(|e| HkError::internal(e.to_string()))
}

fn want<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, HkError> {
    serde_json::from_value(v)
        .map_err(|e| HkError::internal(format!("unexpected Studio reply: {e}")))
}

fn parse(s: &str, what: &str) -> Result<Value, HkError> {
    if s.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(s).map_err(|e| HkError::invalid(format!("{what}: bad JSON: {e}")))
}

/// Is ffmpeg usable where the Studio runs? `{ffmpeg: bool, ffprobe: bool, version: string|null}`.
pub async fn studio_check_tools() -> Result<String, HkError> {
    rpc_json("check_tools", json!({})).await
}

/// True when the Studio runs on a connected instance (files must be uploaded / downloaded).
#[flutter_rust_bridge::frb(sync)]
pub fn studio_is_remote() -> Result<bool, HkError> {
    Ok(core()?.studio_is_remote())
}

/// Studio preferences plus which keys are set.
pub async fn studio_get_settings() -> Result<String, HkError> {
    rpc_json("get_settings", json!({})).await
}

/// Save preferences (`StudioSettings` JSON).
pub async fn studio_save_settings(settings_json: String) -> Result<(), HkError> {
    rpc(
        "save_settings",
        json!({"settings": parse(&settings_json, "settings")?}),
    )
    .await?;
    Ok(())
}

/// `which`: `stt` | `pexels` | `pixabay` | `youtube_secret` | `youtube_refresh`. Empty / `None` deletes the key. Never returned.
pub async fn studio_set_key(which: String, key: Option<String>) -> Result<(), HkError> {
    rpc("set_key", json!({"which": which, "key": key})).await?;
    Ok(())
}

/// Local transcription: `{helper: bool, models: [{id, name, size_bytes, note, installed,
/// downloading, progress: {done, total}|null, error}]}`. `helper` is false when this install has
/// no `hk-whisper` next to it (then use an OpenAI-compatible endpoint or an instance).
pub async fn studio_whisper_status() -> Result<String, HkError> {
    rpc_json("whisper_status", json!({})).await
}

/// Download a speech model in the background (poll `studio_whisper_status`).
pub async fn studio_whisper_pull(id: String) -> Result<(), HkError> {
    rpc("whisper_pull", json!({"id": id})).await?;
    Ok(())
}

pub async fn studio_whisper_cancel(id: String) -> Result<(), HkError> {
    rpc("whisper_cancel", json!({"id": id})).await?;
    Ok(())
}

pub async fn studio_whisper_remove(id: String) -> Result<(), HkError> {
    rpc("whisper_remove", json!({"id": id})).await?;
    Ok(())
}

/// Tools Studio needs (`ffmpeg`, `concat`): `{tools: [{id, name, installed, path, version, source,
/// can_install, note, installing, phase, progress: [done, total]|null, error}]}`.
pub async fn studio_deps_status() -> Result<String, HkError> {
    rpc_json("deps_status", json!({})).await
}

/// Download and install a missing tool where the Studio runs (poll `studio_deps_status`).
pub async fn studio_deps_install(id: String) -> Result<(), HkError> {
    rpc("deps_install", json!({"id": id})).await?;
    Ok(())
}

pub async fn studio_deps_cancel(id: String) -> Result<(), HkError> {
    rpc("deps_cancel", json!({"id": id})).await?;
    Ok(())
}

/// Build the clip's Concat project from its current edit and (with `open`) start the Concat
/// window on it. Returns `{project, opened}`. `rebuild` discards edits made in Concat.
pub async fn studio_concat_edit(
    id: String,
    clip: String,
    open: bool,
    rebuild: bool,
) -> Result<String, HkError> {
    rpc_json(
        "concat_edit",
        json!({"id": id, "clip": clip, "open": open, "rebuild": rebuild}),
    )
    .await
}

/// Export the clip's Concat project with Concat's engine; progress and completion arrive on
/// `studio_events` like a normal render, and the result becomes the clip's rendered video.
pub async fn studio_concat_render(id: String, clip: String) -> Result<String, HkError> {
    want(rpc("concat_render", json!({"id": id, "clip": clip})).await?)
}

/// Built-in templates and caption presets: `{templates: [...], caption_presets: [...]}`.
pub async fn studio_templates() -> Result<String, HkError> {
    rpc_json("templates", json!({})).await
}

pub async fn studio_projects() -> Result<String, HkError> {
    rpc_json("projects", json!({})).await
}

/// Import a video file that is already where the Studio runs (use `studio_upload` first on a
/// remote instance); returns the project JSON.
pub async fn studio_import(path: String, name: Option<String>) -> Result<String, HkError> {
    rpc_json("import", json!({"path": path, "name": name})).await
}

pub async fn studio_project(id: String) -> Result<String, HkError> {
    rpc_json("project", json!({"id": id})).await
}

pub async fn studio_delete(id: String) -> Result<(), HkError> {
    rpc("delete", json!({"id": id})).await?;
    Ok(())
}

/// Transcribe the source (minutes for long videos). The editor stays usable meanwhile.
pub async fn studio_transcribe(id: String) -> Result<(), HkError> {
    rpc("transcribe", json!({"id": id})).await?;
    Ok(())
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
    rpc_json(
        "analyze",
        json!({"id": id, "count": count, "min_secs": min_secs, "max_secs": max_secs, "brief": brief, "template": template}),
    )
    .await
}

/// Apply edit operations (`EditOp` JSON array). `by` = `ui` | `agent`. Returns the new revision.
pub async fn studio_edit(id: String, ops_json: String, by: String) -> Result<u64, HkError> {
    want(
        rpc(
            "edit",
            json!({"id": id, "ops": parse(&ops_json, "ops")?, "by": by}),
        )
        .await?,
    )
}

pub async fn studio_undo(id: String) -> Result<bool, HkError> {
    want(rpc("undo", json!({"id": id})).await?)
}

pub async fn studio_redo(id: String) -> Result<bool, HkError> {
    want(rpc("redo", json!({"id": id})).await?)
}

/// `{can_undo, can_redo, rev}`.
pub async fn studio_history(id: String) -> Result<String, HkError> {
    rpc_json("history", json!({"id": id})).await
}

/// Player-ready plan of a clip's current edit: kept ranges, words, captions, crop path, B-roll.
pub async fn studio_preview(id: String, clip: String) -> Result<String, HkError> {
    rpc_json("preview", json!({"id": id, "clip": clip})).await
}

/// Follow the speaker / subject and crop to vertical (or `square`). Returns the number of points.
pub async fn studio_reframe(id: String, clip: String, square: bool) -> Result<u32, HkError> {
    want(rpc("reframe", json!({"id": id, "clip": clip, "square": square})).await?)
}

/// Download the face-tracking model once (otherwise motion tracking is used).
pub async fn studio_prepare_face_model() -> Result<bool, HkError> {
    want(rpc("prepare_face_model", json!({})).await?)
}

pub async fn studio_broll(id: String, clip: String, max: u32) -> Result<u32, HkError> {
    want(rpc("broll", json!({"id": id, "clip": clip, "max": max})).await?)
}

/// Titles / descriptions / hashtags. `platforms_json`: `["tiktok", ...]` (empty = all).
pub async fn studio_social(
    id: String,
    clip: String,
    platforms_json: String,
    tone: Option<String>,
    language: Option<String>,
) -> Result<(), HkError> {
    rpc(
        "social",
        json!({"id": id, "clip": clip, "platforms": parse(&platforms_json, "platforms")?, "tone": tone, "language": language}),
    )
    .await?;
    Ok(())
}

/// Start rendering in the background; progress / completion arrive on `studio_events`.
pub async fn studio_render(id: String, clip: String) -> Result<String, HkError> {
    want(rpc("render", json!({"id": id, "clip": clip})).await?)
}

pub async fn studio_cancel_render(job: String) -> Result<bool, HkError> {
    want(rpc("cancel_render", json!({"job": job})).await?)
}

/// Path of the rendered MP4 where the Studio runs, if the clip was rendered.
pub async fn studio_render_path(id: String, clip: String) -> Result<Option<String>, HkError> {
    want(rpc("render_path", json!({"id": id, "clip": clip})).await?)
}

/// Upload the rendered clip to YouTube. `privacy`: `private` (default) | `unlisted` | `public`.
/// Returns the video URL.
pub async fn studio_publish_youtube(
    id: String,
    clip: String,
    privacy: String,
) -> Result<String, HkError> {
    want(
        rpc(
            "publish_youtube",
            json!({"id": id, "clip": clip, "privacy": privacy}),
        )
        .await?,
    )
}

/// Write clip.mp4 + captions + per-platform post text into `dir` (on the machine running the
/// Studio). Returns the folder.
pub async fn studio_export(id: String, clip: String, dir: String) -> Result<String, HkError> {
    want(rpc("export", json!({"id": id, "clip": clip, "dir": dir})).await?)
}

/// JPEG frame of the source at `at` seconds (path where the Studio runs).
pub async fn studio_thumbnail(id: String, at: f64) -> Result<String, HkError> {
    want(rpc("thumbnail", json!({"id": id, "at": at})).await?)
}

/// Low-res proxy of the source for smooth scrubbing (made once; may take a while).
pub async fn studio_proxy(id: String) -> Result<String, HkError> {
    want(rpc("proxy", json!({"id": id})).await?)
}

/// What a media player can open for a path from a Studio result: the path on this device, or a
/// short-lived authenticated URL for a connected instance.
pub async fn studio_media(path: String) -> Result<String, HkError> {
    core()?.studio_media(&path).await
}

/// Copy a local video to where the Studio runs. Emits `{"sent":N,"total":N}` progress JSON and
/// finally `{"path":"..."}` (the path to pass to `studio_import`).
pub async fn studio_upload(local_path: String, sink: StreamSink<String>) -> Result<(), HkError> {
    let progress = |sent: u64, total: u64| {
        let _ = sink.add(format!(r#"{{"sent":{sent},"total":{total}}}"#));
    };
    let server_path = core()?.studio_upload(&local_path, &progress).await?;
    sink.add(json!({"path": server_path}).to_string())
        .map_err(|e| HkError::new(HkErrorKind::Closed, e.to_string()))
}

/// Copy a rendered clip from where the Studio runs to `dest` on this device.
pub async fn studio_download(server_path: String, dest: String) -> Result<(), HkError> {
    core()?.studio_download(&server_path, &dest).await
}

/// Live change feed of one project as JSON `StudioEvent`s (edits by you or the agent, transcript
/// ready, clips added, render progress). A `{"kind":"resync"}` event means events were missed:
/// reload the project.
pub async fn studio_events(id: String, sink: StreamSink<String>) -> Result<(), HkError> {
    let mut stream = core()?.studio_events(&id).await?;
    while let Some(msg) = stream.next().await {
        sink.add(msg)
            .map_err(|e| HkError::new(HkErrorKind::Closed, e.to_string()))?;
    }
    Ok(())
}
