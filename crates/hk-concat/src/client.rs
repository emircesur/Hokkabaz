//! [`ConcatClient`]: typed wrapper over [`ConcatBackend`]. Every method is one Concat MCP tool
//! (names verified against `concat-mcp/src/tools.rs`); argument fields are Concat's camelCase.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::backend::{ConcatBackend, ToolReply};
use crate::config::RootsPolicy;
use crate::types::{
    ExportJob, ExportSpec, ExportState, ExportStatus, FrameFile, ProjectInfo, Summary, VersionInfo,
    VideoSettings,
};
use crate::{ConcatConfig, ConcatError};

/// Properties `keyframe_set` animates on a clip (`KeyProperty` in concat-project).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyProperty {
    Scale,
    OffsetX,
    OffsetY,
    Rotation,
    Opacity,
    Volume,
}

impl KeyProperty {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scale => "scale",
            Self::OffsetX => "offsetX",
            Self::OffsetY => "offsetY",
            Self::Rotation => "rotation",
            Self::Opacity => "opacity",
            Self::Volume => "volume",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "scale" => Self::Scale,
            "offsetX" => Self::OffsetX,
            "offsetY" => Self::OffsetY,
            "rotation" => Self::Rotation,
            "opacity" => Self::Opacity,
            "volume" => Self::Volume,
            _ => return None,
        })
    }
}

/// Which clip edge `clip_trim` moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// Whole picture visible, bars where shapes differ.
    Fit,
    /// Covers the frame, crops the overhang.
    Fill,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClipMove {
    pub clip_id: String,
    pub start: f64,
    pub track_id: String,
}

/// One caption line on the timeline (seconds on the timeline).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CaptionLine {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct ConcatClient {
    backend: Arc<dyn ConcatBackend>,
    roots: Option<RootsPolicy>,
}

fn path_str(p: &str) -> Value {
    Value::String(p.to_owned())
}

impl ConcatClient {
    pub fn new(backend: Arc<dyn ConcatBackend>) -> Self {
        Self {
            backend,
            roots: None,
        }
    }

    /// Checks write targets (project folder, export file, frame file) against Concat's confinement
    /// before calling, so the model gets the fix in the error without a round trip.
    #[must_use]
    pub fn with_roots(mut self, roots: Option<RootsPolicy>) -> Self {
        self.roots = roots;
        self
    }

    /// Starts the backend `cfg` selects. Desktop `Process` backend spawns `concat --mcp`.
    pub async fn connect(cfg: &ConcatConfig) -> Result<Self, ConcatError> {
        let backend: Arc<dyn ConcatBackend> = match cfg.backend {
            crate::BackendKind::Process => Arc::new(crate::ProcessBackend::spawn(cfg).await?),
            #[cfg(feature = "engine")]
            crate::BackendKind::Engine => Arc::new(crate::engine::EngineBackend::start_with_roots(
                cfg.roots.clone(),
            )?),
            #[cfg(not(feature = "engine"))]
            crate::BackendKind::Engine => {
                return Err(ConcatError::Unsupported(
                    "built without the `engine` cargo feature of hk-concat",
                ));
            }
        };
        Ok(Self::new(backend).with_roots(cfg.effective_roots()))
    }

    pub fn backend(&self) -> &Arc<dyn ConcatBackend> {
        &self.backend
    }

    pub async fn shutdown(&self) {
        self.backend.shutdown().await;
    }

    fn check_write(&self, path: &str) -> Result<(), ConcatError> {
        match &self.roots {
            Some(r) => r.check(path),
            None => Ok(()),
        }
    }

    /// Raw tool call; the escape hatch for tools without a typed method.
    pub async fn call(&self, tool: &str, args: Value) -> Result<ToolReply, ConcatError> {
        self.backend.call_tool(tool, args).await
    }

    async fn typed<T: DeserializeOwned>(&self, tool: &str, args: Value) -> Result<T, ConcatError> {
        let reply = self.backend.call_tool(tool, args).await?;
        serde_json::from_value(reply.value).map_err(|e| ConcatError::decode(tool, e))
    }

    async fn summary(&self, tool: &str, args: Value) -> Result<Summary, ConcatError> {
        self.typed(tool, args).await
    }

    // ------------------------------------------------------------------ health

    pub async fn version(&self) -> Result<VersionInfo, ConcatError> {
        self.typed("concat_version", json!({})).await
    }

    /// `version()` that also proves a project round trip is possible; cheap liveness probe.
    pub async fn health(&self) -> Result<VersionInfo, ConcatError> {
        let v = self.version().await?;
        self.project_list().await?;
        Ok(v)
    }

    // ---------------------------------------------------------------- projects

    pub async fn project_list(&self) -> Result<Vec<ProjectInfo>, ConcatError> {
        self.typed("project_list", json!({})).await
    }

    /// Creates `location/name` and opens it. `location` must exist and be writable.
    pub async fn project_create(
        &self,
        location: &str,
        name: &str,
        video: Option<VideoSettings>,
    ) -> Result<Summary, ConcatError> {
        self.check_write(location)?;
        self.summary(
            "project_create",
            json!({ "location": location, "name": name, "video": video }),
        )
        .await
    }

    pub async fn project_open(&self, path: &str) -> Result<Summary, ConcatError> {
        self.summary("project_open", json!({ "path": path })).await
    }

    pub async fn project_save(&self, path: &str, rename: Option<&str>) -> Result<(), ConcatError> {
        self.backend
            .call_tool("project_save", json!({ "path": path, "name": rename }))
            .await
            .map(drop)
    }

    pub async fn project_close(&self, path: &str, save: bool) -> Result<(), ConcatError> {
        self.backend
            .call_tool("project_close", json!({ "path": path, "save": save }))
            .await
            .map(drop)
    }

    pub async fn project_set_video(
        &self,
        path: &str,
        video: VideoSettings,
    ) -> Result<Summary, ConcatError> {
        self.summary("project_set_video", json!({ "path": path, "video": video }))
            .await
    }

    /// Compact reading: lanes, clips in time order, media bin, ids.
    pub async fn timeline_read(&self, path: &str) -> Result<Summary, ConcatError> {
        self.summary("timeline_read", json!({ "path": path })).await
    }

    /// Full project document view (`project.get`); needed for keyframe values.
    pub async fn project_get(&self, path: &str) -> Result<Value, ConcatError> {
        Ok(self
            .backend
            .call_tool("project_get", json!({ "path": path }))
            .await?
            .value)
    }

    // ------------------------------------------------------------------- media

    pub async fn media_probe(&self, file: &str) -> Result<Value, ConcatError> {
        Ok(self
            .backend
            .call_tool("media_probe", json!({ "path": file }))
            .await?
            .value)
    }

    /// Adds `file` (absolute path) to the bin. Returns the new media id (`createdId`); importing a
    /// path already in the bin changes nothing, so the existing id is looked up by path instead.
    pub async fn import_media(&self, project: &str, file: &str) -> Result<String, ConcatError> {
        let s = self
            .summary("media_import", json!({ "path": project, "file": file }))
            .await?;
        if let Some(id) = s.created_id.as_deref() {
            return Ok(id.to_owned());
        }
        s.media
            .iter()
            .find(|m| m.path == file)
            .map(|m| m.id.clone())
            .ok_or_else(|| {
                ConcatError::decode("media_import", "no createdId and file not in media bin")
            })
    }

    // ---------------------------------------------------------- cut / arrange

    /// Places a clip of `media_id` on `track_id` at `start` seconds. Returns the clip id.
    pub async fn add_clip(
        &self,
        project: &str,
        media_id: &str,
        track_id: &str,
        start: f64,
        ripple: bool,
    ) -> Result<String, ConcatError> {
        let s = self
            .summary(
                "clip_add",
                json!({ "path": project, "mediaId": media_id, "trackId": track_id, "start": start, "ripple": ripple }),
            )
            .await?;
        Ok(s.created("clip_add")?.to_owned())
    }

    /// Cuts each clip in two at timeline `time`; the tail gets a new id (read it from the timeline).
    pub async fn split(
        &self,
        project: &str,
        clip_ids: &[String],
        time: f64,
    ) -> Result<Summary, ConcatError> {
        self.summary(
            "clips_split",
            json!({ "path": project, "clipIds": clip_ids, "time": time }),
        )
        .await
    }

    /// Moves a clip edge by `delta` timeline seconds (`Start` + positive = shorten from the head).
    pub async fn trim(
        &self,
        project: &str,
        clip_id: &str,
        edge: Edge,
        delta: f64,
        ripple: bool,
    ) -> Result<Summary, ConcatError> {
        let edge = match edge {
            Edge::Start => "start",
            Edge::End => "end",
        };
        self.summary(
            "clip_trim",
            json!({ "path": project, "clipId": clip_id, "edge": edge, "delta": delta, "ripple": ripple }),
        )
        .await
    }

    pub async fn remove_clips(
        &self,
        project: &str,
        clip_ids: &[String],
        ripple: bool,
    ) -> Result<Summary, ConcatError> {
        self.summary(
            "clips_remove",
            json!({ "path": project, "clipIds": clip_ids, "ripple": ripple }),
        )
        .await
    }

    pub async fn move_clips(
        &self,
        project: &str,
        moves: &[ClipMove],
    ) -> Result<Summary, ConcatError> {
        let moves: Vec<Value> = moves
            .iter()
            .map(|m| json!({ "clipId": m.clip_id, "start": m.start, "trackId": m.track_id }))
            .collect();
        self.summary("clips_move", json!({ "path": project, "moves": moves }))
            .await
    }

    /// Adds a lane (top, or bottom). Returns its id.
    pub async fn add_track(&self, project: &str, bottom: bool) -> Result<String, ConcatError> {
        let s = self
            .summary("track_add", json!({ "path": project, "bottom": bottom }))
            .await?;
        Ok(s.created("track_add")?.to_owned())
    }

    /// Deletes the timeline range `[from, to)` on every lane and closes the gap (ripple): split
    /// everything at `to` then `from`, delete the pieces inside. A lane with no piece inside the
    /// range does not shift, so run this before adding captions/overlays, not after.
    pub async fn cut_range(
        &self,
        project: &str,
        from: f64,
        to: f64,
    ) -> Result<Summary, ConcatError> {
        const EPS: f64 = 1e-3;
        if !(to > from && from >= 0.0) {
            return Err(ConcatError::InvalidArgs(format!(
                "cut range [{from}, {to}) is empty or negative"
            )));
        }
        for t in [to, from] {
            let s = self.timeline_read(project).await?;
            let ids: Vec<String> = s.timeline.clips.iter().map(|c| c.id.clone()).collect();
            if !ids.is_empty() {
                self.split(project, &ids, t).await?;
            }
        }
        let s = self.timeline_read(project).await?;
        let inside: Vec<String> = s
            .timeline
            .clips
            .iter()
            .filter(|c| c.start >= from - EPS && c.end <= to + EPS)
            .map(|c| c.id.clone())
            .collect();
        if inside.is_empty() {
            return Ok(s);
        }
        self.remove_clips(project, &inside, true).await
    }

    pub async fn clip_speed(
        &self,
        project: &str,
        clip_id: &str,
        speed: f64,
    ) -> Result<Summary, ConcatError> {
        self.summary(
            "clip_speed",
            json!({ "path": project, "clipId": clip_id, "speed": speed }),
        )
        .await
    }

    // ------------------------------------------------------- titles / captions

    /// Places a title. `style` is Concat's `TextStyle` (only fields that differ from default).
    pub async fn add_title(
        &self,
        project: &str,
        text: &str,
        start: f64,
        duration: f64,
        style: Option<Value>,
        offset_y: Option<f64>,
    ) -> Result<String, ConcatError> {
        let mut style = style.unwrap_or_else(|| json!({}));
        style["content"] = Value::String(text.to_owned());
        let s = self
            .summary(
                "title_add",
                json!({
                    "path": project, "start": start, "duration": duration, "above": true,
                    "style": style, "offsetY": offset_y,
                }),
            )
            .await?;
        Ok(s.created("title_add")?.to_owned())
    }

    /// One undoable step: a caption (title) per line, each on a lane above the picture.
    pub async fn add_captions(
        &self,
        project: &str,
        lines: &[CaptionLine],
        style: Option<Value>,
        offset_y: Option<f64>,
    ) -> Result<Summary, ConcatError> {
        if lines.is_empty() {
            return Err(ConcatError::InvalidArgs("no caption lines".into()));
        }
        let mut args = json!({ "path": project, "captions": lines });
        if let Some(s) = style {
            args["style"] = s;
        }
        if let Some(y) = offset_y {
            args["offsetY"] = json!(y);
        }
        self.summary("captions_add", args).await
    }

    // ------------------------------------------------------ keyframes / frame

    /// `at` is a fraction of the clip (0..=1); `value` in the property's own units.
    pub async fn set_keyframe(
        &self,
        project: &str,
        clip_id: &str,
        property: KeyProperty,
        at: f64,
        value: f64,
        ease: Option<[f64; 4]>,
    ) -> Result<Summary, ConcatError> {
        let mut args = json!({
            "path": project, "clipId": clip_id, "property": property.as_str(), "at": at, "value": value,
        });
        if let Some(e) = ease {
            args["ease"] = json!(e);
        }
        self.summary("keyframe_set", args).await
    }

    pub async fn clear_keyframe(
        &self,
        project: &str,
        clip_id: &str,
        property: KeyProperty,
        at: f64,
    ) -> Result<Summary, ConcatError> {
        self.summary(
            "keyframe_clear",
            json!({ "path": project, "clipId": clip_id, "property": property.as_str(), "at": at }),
        )
        .await
    }

    /// Static placement; only given fields change. `scale` 1 fits the frame; offsets are frame fractions.
    pub async fn set_transform(
        &self,
        project: &str,
        clip_id: &str,
        scale: Option<f64>,
        offset_x: Option<f64>,
        offset_y: Option<f64>,
        rotation: Option<f64>,
    ) -> Result<Summary, ConcatError> {
        self.summary(
            "clip_transform",
            json!({
                "path": project, "clipId": clip_id, "scale": scale,
                "offsetX": offset_x, "offsetY": offset_y, "rotation": rotation,
            }),
        )
        .await
    }

    pub async fn frame_clip(
        &self,
        project: &str,
        clip_id: &str,
        framing: Framing,
        at: f64,
    ) -> Result<Summary, ConcatError> {
        let framing = match framing {
            Framing::Fit => "fit",
            Framing::Fill => "fill",
        };
        self.summary(
            "clip_frame",
            json!({ "path": project, "clipId": clip_id, "framing": framing, "at": at }),
        )
        .await
    }

    // -------------------------------------------------------- effects / looks

    /// Effect packages (id, kind, params). `kind`: effect|filter|audio|transition|generator.
    pub async fn effects_list(&self, kind: Option<&str>) -> Result<Value, ConcatError> {
        Ok(self
            .backend
            .call_tool("effects_list", json!({ "kind": kind }))
            .await?
            .value)
    }

    /// Replaces the clip's whole video-effect chain: `[{ "id": "concat.warm", "params": {...} }]`.
    pub async fn set_effects(
        &self,
        project: &str,
        clip_id: &str,
        chain: Value,
    ) -> Result<Summary, ConcatError> {
        self.clip_update(project, clip_id, json!({ "videoEffects": chain }))
            .await
    }

    /// Transition into the clip (on the cut before it); `None` removes it.
    pub async fn set_transition(
        &self,
        project: &str,
        clip_id: &str,
        transition: Option<(&str, f64)>,
    ) -> Result<Summary, ConcatError> {
        let v = transition.map_or(Value::Null, |(id, d)| json!({ "id": id, "duration": d }));
        self.clip_update(project, clip_id, json!({ "transitionIn": v }))
            .await
    }

    /// An effect/look as a layer over the timeline from `start` for `duration` seconds.
    pub async fn add_effect_layer(
        &self,
        project: &str,
        effect_id: &str,
        name: &str,
        start: f64,
        duration: f64,
    ) -> Result<String, ConcatError> {
        let s = self
            .summary(
                "effect_layer_add",
                json!({
                    "path": project, "start": start, "duration": duration, "effectId": effect_id,
                    "name": name, "trackId": null,
                }),
            )
            .await?;
        Ok(s.created("effect_layer_add")?.to_owned())
    }

    /// Field-wise clip patch (`volume`, `opacity`, `crop`, `videoEffects`, `transitionIn`, ...).
    pub async fn clip_update(
        &self,
        project: &str,
        clip_id: &str,
        patch: Value,
    ) -> Result<Summary, ConcatError> {
        self.summary(
            "clip_update",
            json!({ "path": project, "clipId": clip_id, "patch": patch }),
        )
        .await
    }

    /// Any edit command (`{"op": "...", ...}`) or `{"op":"batch","commands":[...]}` as one undo step.
    pub async fn edit_apply(&self, project: &str, command: Value) -> Result<Summary, ConcatError> {
        self.summary("edit_apply", json!({ "path": project, "command": command }))
            .await
    }

    pub async fn undo(&self, project: &str) -> Result<Summary, ConcatError> {
        self.summary("edit_undo", json!({ "path": project })).await
    }

    pub async fn redo(&self, project: &str) -> Result<Summary, ConcatError> {
        self.summary("edit_redo", json!({ "path": project })).await
    }

    // ----------------------------------------------------------------- preview

    /// Renders the composited frame at `time` into a PNG at `output` (write target: must be under
    /// the allowed roots). Inline images are not used: they would be dropped by text-only callers.
    pub async fn get_frame(
        &self,
        project: &str,
        time: f64,
        output: &str,
        size: Option<(u32, u32)>,
    ) -> Result<FrameFile, ConcatError> {
        self.check_write(output)?;
        let mut args = json!({ "path": project, "time": time, "output": output });
        if let Some((w, h)) = size {
            args["width"] = json!(w);
            args["height"] = json!(h);
        }
        self.typed("preview_frame", args).await
    }

    // ------------------------------------------------------------------ export

    pub async fn export_start(
        &self,
        project: &str,
        spec: &ExportSpec,
    ) -> Result<ExportJob, ConcatError> {
        self.check_write(&spec.output)?;
        let mut args =
            serde_json::to_value(spec).map_err(|e| ConcatError::InvalidArgs(e.to_string()))?;
        args["path"] = path_str(project);
        self.typed("export_start", args).await
    }

    pub async fn export_status(&self, job: &str) -> Result<ExportStatus, ConcatError> {
        self.typed("export_status", json!({ "job": job })).await
    }

    pub async fn export_cancel(&self, job: &str) -> Result<(), ConcatError> {
        self.backend
            .call_tool("export_cancel", json!({ "job": job }))
            .await
            .map(drop)
    }

    /// Polls `export_status` until the job leaves `running` (any terminal state is returned as
    /// `Ok`; check `state`) or `max` elapses (`Timeout`; the job keeps running, call again or
    /// cancel). Polling instead of Concat's blocking `export_wait` keeps the stdio server free for
    /// `export_cancel`.
    pub async fn export_wait(
        &self,
        job: &str,
        poll: Duration,
        max: Duration,
        mut on_progress: impl FnMut(&ExportStatus) + Send,
    ) -> Result<ExportStatus, ConcatError> {
        let deadline = Instant::now() + max;
        loop {
            let st = self.export_status(job).await?;
            on_progress(&st);
            if st.state != ExportState::Running {
                return Ok(st);
            }
            if Instant::now() >= deadline {
                return Err(ConcatError::Timeout(max));
            }
            tokio::time::sleep(poll).await;
        }
    }
}
