//! `concat_*` agent tools over [`ConcatClient`].
//!
//! Chosen over a plain pass-through of Concat's ~45 MCP tools (an `hk-mcp` config entry named
//! `concat` still works for that, see docs/CONCAT.md) because: the Studio agent needs composite
//! operations Concat has no single tool for (`concat_apply_plan`, `cut_range`, `reframe`, polling
//! export), 11 action-style tools cost the model far fewer schema tokens than 45, and risk
//! classification (approval gate) is per action here. `concat_raw` keeps the full surface reachable.
//!
//! Lives in hk-concat (hk-tools is android-safe); `hk-server` feature `concat` only re-exports
//! [`concat_tools`] so the daemon registers them with `AgentRuntime::add_tools`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hk_tools::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use crate::client::{CaptionLine, ClipMove, ConcatClient, Edge, Framing, KeyProperty};
use crate::plan::{self, CaptionStyle, StudioClipPlan, TimedWord};
use crate::types::{ExportPreset, VideoSettings};
use crate::{ConcatConfig, ConcatError};

const MAX_WAIT_SECS: u64 = 1800;
const DEFAULT_WAIT_SECS: u64 = 120;

/// Lazily started, self-healing connection to Concat shared by all `concat_*` tools. The editor
/// process is only spawned when a tool first needs it, and respawned after it dies.
pub struct ConcatSession {
    cfg: ConcatConfig,
    client: Mutex<Option<ConcatClient>>,
    /// A client supplied by the caller (tests, in-process engine): never respawned.
    fixed: bool,
}

impl std::fmt::Debug for ConcatSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConcatSession").field("cfg", &self.cfg).finish_non_exhaustive()
    }
}

impl ConcatSession {
    pub fn new(cfg: ConcatConfig) -> Arc<Self> {
        Arc::new(Self { cfg, client: Mutex::new(None), fixed: false })
    }

    pub fn with_client(client: ConcatClient) -> Arc<Self> {
        Arc::new(Self { cfg: ConcatConfig::default(), client: Mutex::new(Some(client)), fixed: true })
    }

    pub async fn client(&self) -> Result<ConcatClient, ConcatError> {
        let mut g = self.client.lock().await;
        if let Some(c) = g.as_ref() {
            return Ok(c.clone());
        }
        let c = ConcatClient::connect(&self.cfg).await?;
        *g = Some(c.clone());
        Ok(c)
    }

    /// Drops a dead connection so the next call reconnects.
    async fn reset(&self) {
        if self.fixed {
            return;
        }
        if let Some(c) = self.client.lock().await.take() {
            c.shutdown().await;
        }
    }

    /// Stops the editor process (daemon shutdown).
    pub async fn shutdown(&self) {
        if let Some(c) = self.client.lock().await.take() {
            c.shutdown().await;
        }
    }
}

/// The tool set, ready for `AgentRuntime::add_tools`.
pub fn concat_tools(session: Arc<ConcatSession>) -> Vec<Arc<dyn Tool>> {
    Kind::ALL
        .iter()
        .map(|k| Arc::new(ConcatTool { session: session.clone(), kind: *k }) as Arc<dyn Tool>)
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Status,
    Project,
    Media,
    Cut,
    Text,
    Keyframe,
    Effect,
    Preview,
    Export,
    ApplyPlan,
    Raw,
}

impl Kind {
    const ALL: [Kind; 11] = [
        Kind::Status,
        Kind::Project,
        Kind::Media,
        Kind::Cut,
        Kind::Text,
        Kind::Keyframe,
        Kind::Effect,
        Kind::Preview,
        Kind::Export,
        Kind::ApplyPlan,
        Kind::Raw,
    ];

    fn name(self) -> &'static str {
        match self {
            Kind::Status => "concat_status",
            Kind::Project => "concat_project",
            Kind::Media => "concat_media",
            Kind::Cut => "concat_cut",
            Kind::Text => "concat_text",
            Kind::Keyframe => "concat_keyframe",
            Kind::Effect => "concat_effect",
            Kind::Preview => "concat_preview",
            Kind::Export => "concat_export",
            Kind::ApplyPlan => "concat_apply_plan",
            Kind::Raw => "concat_raw",
        }
    }
}

struct ConcatTool {
    session: Arc<ConcatSession>,
    kind: Kind,
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

fn action(values: &[&str], doc: &str) -> Value {
    json!({ "type": "string", "enum": values, "description": doc })
}

const PATH: &str = "Project folder (the `path` from project_create/project_list).";
const TIMES: &str = "Times are seconds on the timeline; keyframe `at` is a fraction (0..1) of the clip; sizes/offsets are fractions of the frame.";

fn spec_of(kind: Kind) -> (&'static str, Value) {
    match kind {
        Kind::Status => (
            "Check the Concat video editor bridge: version, backend, which folders it may write to. Call first; fails with install instructions when Concat is missing.",
            obj(json!({}), &[]),
        ),
        Kind::Project => (
            "Concat projects. list: recent projects. create: new project folder `location/name` (location must exist and be under the home folder) with `format` vertical|landscape|square (default vertical 1080x1920@30). open/save/close(save?)/read (timeline summary with all ids)/as_plan (read the project back as a Studio clip plan). Edits live in memory until save.",
            obj(
                json!({
                    "action": action(&["list", "create", "open", "save", "close", "read", "as_plan"], "What to do."),
                    "path": {"type": "string", "description": PATH},
                    "location": {"type": "string", "description": "create: existing directory to make the project folder in."},
                    "name": {"type": "string", "description": "create: project name; save: optional rename."},
                    "format": {"type": "string", "enum": ["vertical", "landscape", "square"]},
                    "save": {"type": "boolean", "description": "close: save first."}
                }),
                &["action"],
            ),
        ),
        Kind::Media => (
            "Media files. probe: duration/streams of a file without a project. import: add an absolute file path to a project's media bin; returns `media_id` (place it with concat_cut add_clip).",
            obj(
                json!({
                    "action": action(&["probe", "import"], "What to do."),
                    "path": {"type": "string", "description": "import: project folder."},
                    "file": {"type": "string", "description": "Absolute media file path."}
                }),
                &["action", "file"],
            ),
        ),
        Kind::Cut => (
            "Arrange and cut clips (all in-memory, undoable). add_clip(media_id, track_id, start, ripple?), split(clip_ids, time), trim(clip_id, edge start|end, delta seconds; start+positive shortens the head; ripple?), delete(clip_ids, ripple?), move(moves:[{clip_id,start,track_id}]), cut_range(from,to: remove that timeline span on every lane and close the gap), speed(clip_id, speed 0.0625-16), add_track(bottom?), undo, redo. Read ids with concat_project read.",
            obj(
                json!({
                    "action": action(&["add_clip", "split", "trim", "delete", "move", "cut_range", "speed", "add_track", "undo", "redo"], "What to do."),
                    "path": {"type": "string", "description": PATH},
                    "media_id": {"type": "string"}, "track_id": {"type": "string"}, "clip_id": {"type": "string"},
                    "clip_ids": {"type": "array", "items": {"type": "string"}},
                    "start": {"type": "number"}, "time": {"type": "number"}, "from": {"type": "number"}, "to": {"type": "number"},
                    "edge": {"type": "string", "enum": ["start", "end"]}, "delta": {"type": "number"},
                    "speed": {"type": "number"}, "ripple": {"type": "boolean"}, "bottom": {"type": "boolean"},
                    "moves": {"type": "array", "items": {"type": "object", "properties": {
                        "clip_id": {"type": "string"}, "start": {"type": "number"}, "track_id": {"type": "string"}
                    }, "required": ["clip_id", "start", "track_id"]}}
                }),
                &["action", "path"],
            ),
        ),
        Kind::Text => (
            "Titles and captions. title(text,start,duration,style?,offset_y?). captions(lines:[{start,end,text}], style?, offset_y?): one caption per line. captions_from_words(words:[{text,start,end}] in TIMELINE seconds, style fields + max_words_per_line/max_chars_per_line/uppercase, offset_y): groups transcript words into lines then adds them (for source-time words use concat_apply_plan). `style` = Concat TextStyle (fontFamily, fontSize as fraction of frame height, fontWeight, color \"#rrggbb\", strokeColor, strokeWidth, background). offset_y: fraction of frame height from centre, positive down (0.3 = lower third).",
            obj(
                json!({
                    "action": action(&["title", "captions", "captions_from_words"], "What to do."),
                    "path": {"type": "string", "description": PATH},
                    "text": {"type": "string"}, "start": {"type": "number"}, "duration": {"type": "number"},
                    "style": {"type": "object"}, "offset_y": {"type": "number"},
                    "lines": {"type": "array", "items": {"type": "object", "properties": {
                        "start": {"type": "number"}, "end": {"type": "number"}, "text": {"type": "string"}
                    }, "required": ["start", "end", "text"]}},
                    "words": {"type": "array", "items": {"type": "object", "properties": {
                        "text": {"type": "string"}, "start": {"type": "number"}, "end": {"type": "number"}
                    }, "required": ["text", "start", "end"]}},
                    "max_words_per_line": {"type": "integer"}, "max_chars_per_line": {"type": "integer"}, "uppercase": {"type": "boolean"}
                }),
                &["action", "path"],
            ),
        ),
        Kind::Keyframe => (
            "Animate/position a clip. set(clip_id, property scale|offsetX|offsetY|rotation|opacity|volume, at, value, ease?[x1,y1,x2,y2]), clear(clip_id, property, at), transform(clip_id, scale?, offset_x?, offset_y?, rotation?), frame(clip_id, framing fit|fill, at?), reframe(clip_id, points:[{at, center_x, center_y, zoom?}]): smart-crop 16:9 -> 9:16 by keeping the subject (center_x/center_y = fraction of the SOURCE frame, zoom>=1 extra zoom) in frame over time.",
            obj(
                json!({
                    "action": action(&["set", "clear", "transform", "frame", "reframe"], TIMES),
                    "path": {"type": "string", "description": PATH}, "clip_id": {"type": "string"},
                    "property": {"type": "string", "enum": ["scale", "offsetX", "offsetY", "rotation", "opacity", "volume"]},
                    "at": {"type": "number"}, "value": {"type": "number"},
                    "ease": {"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4},
                    "scale": {"type": "number"}, "offset_x": {"type": "number"}, "offset_y": {"type": "number"}, "rotation": {"type": "number"},
                    "framing": {"type": "string", "enum": ["fit", "fill"]},
                    "points": {"type": "array", "items": {"type": "object", "properties": {
                        "at": {"type": "number"}, "center_x": {"type": "number"}, "center_y": {"type": "number"}, "zoom": {"type": "number"}
                    }, "required": ["at", "center_x", "center_y"]}}
                }),
                &["action", "path", "clip_id"],
            ),
        ),
        Kind::Effect => (
            "Effects and transitions. list(kind? effect|filter|audio|transition|generator): packages with ids and parameter ranges. set_effects(clip_id, chain:[{id,params}]): REPLACES the clip's effect chain. transition(clip_id, id?, duration?): transition into the clip (omit id to remove). layer(effect_id, name, start, duration): effect layer over the timeline.",
            obj(
                json!({
                    "action": action(&["list", "set_effects", "transition", "layer"], "What to do."),
                    "path": {"type": "string", "description": PATH}, "clip_id": {"type": "string"},
                    "kind": {"type": "string"}, "chain": {"type": "array", "items": {"type": "object"}},
                    "id": {"type": "string"}, "duration": {"type": "number"}, "effect_id": {"type": "string"},
                    "name": {"type": "string"}, "start": {"type": "number"}
                }),
                &["action"],
            ),
        ),
        Kind::Preview => (
            "Render the composited frame at `time` seconds to a PNG file (titles, effects, crops included) and return its path/size. `output` must be under the home folder; default is inside the project folder.",
            obj(
                json!({
                    "path": {"type": "string", "description": PATH}, "time": {"type": "number"},
                    "output": {"type": "string"}, "width": {"type": "integer"}, "height": {"type": "integer"}
                }),
                &["path", "time"],
            ),
        ),
        Kind::Export => (
            "Render a project to a video file. start(path, output .mp4, preset draft|vertical_1080|landscape_1080|master, crf?, codec?) returns a job; status(job); wait(job, timeout_secs): polls until done/failed/cancelled or the timeout (call again if still running); cancel(job). One export runs at a time.",
            obj(
                json!({
                    "action": action(&["start", "status", "wait", "cancel"], "What to do."),
                    "path": {"type": "string", "description": PATH}, "output": {"type": "string"},
                    "preset": {"type": "string", "enum": ["draft", "vertical_1080", "landscape_1080", "master"]},
                    "crf": {"type": "integer", "minimum": 0, "maximum": 51}, "codec": {"type": "string", "enum": ["h264", "hevc", "av1"]},
                    "job": {"type": "string"}, "timeout_secs": {"type": "integer", "minimum": 1, "maximum": MAX_WAIT_SECS}
                }),
                &["action"],
            ),
        ),
        Kind::ApplyPlan => (
            "Build a whole Concat project from a Studio clip plan in one call: source file, kept source ranges (the cut), transcript words + caption style, reframe path (center_x/center_y/zoom over OUTPUT time), overlays (titles, b-roll). Creates `location/plan.name`. dry_run=true only returns the Concat operations without touching Concat.",
            obj(
                json!({
                    "location": {"type": "string", "description": "Existing directory (under the home folder) for the project folder."},
                    "plan": {"type": "object", "description": "StudioClipPlan: {name, source, kept_ranges:[{start,end}], captions?:{words:[{text,start,end}], style?}, crop_keyframes?:[{time,center_x,center_y,zoom?}], overlays?:[{kind:title|media,...}], video?, framing?}"},
                    "dry_run": {"type": "boolean"}
                }),
                &["plan"],
            ),
        ),
        Kind::Raw => (
            "Call any Concat tool by name (see Concat docs/MCP.md: project_*, clip_*, effects_list, template_*, edit_apply, ...). Arguments are Concat's own (camelCase). Prefer the typed concat_* tools.",
            obj(
                json!({"tool": {"type": "string"}, "arguments": {"type": "object"}}),
                &["tool"],
            ),
        ),
    }
}

#[async_trait]
impl Tool for ConcatTool {
    fn spec(&self) -> ToolSpec {
        let (description, parameters) = spec_of(self.kind);
        ToolSpec { name: self.kind.name().into(), description: description.into(), parameters }
    }

    fn risk(&self, args: &Value) -> Risk {
        let act = args.get("action").and_then(Value::as_str).unwrap_or("");
        match self.kind {
            // Arbitrary Concat tools can write anywhere Concat may; exports write a video file.
            Kind::Raw => Risk::Ask,
            Kind::Export if act == "start" => Risk::Ask,
            Kind::ApplyPlan => {
                if args.get("dry_run").and_then(Value::as_bool).unwrap_or(false) {
                    Risk::Safe
                } else {
                    Risk::Ask
                }
            }
            Kind::Project if matches!(act, "create" | "save" | "close") => Risk::Ask,
            Kind::Preview => Risk::Ask, // writes a PNG
            _ => Risk::Safe,
        }
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let name = self.kind.name();
        let a = args
            .as_object()
            .ok_or_else(|| ToolError::InvalidArgs { tool: name.into(), reason: "arguments must be an object".into() })?;
        // dry_run needs no editor at all.
        let res = if self.kind == Kind::ApplyPlan && a.get("dry_run").and_then(Value::as_bool).unwrap_or(false) {
            dry_run(a)
        } else {
            match self.session.client().await {
                Ok(c) => dispatch(self.kind, &c, a).await,
                Err(e) => Err(e),
            }
        };
        match res {
            Ok(v) => Ok(ToolOutput::ok(serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()))),
            Err(ConcatError::InvalidArgs(r)) => Err(ToolError::InvalidArgs { tool: name.into(), reason: r }),
            Err(e) => {
                if e.is_connection_lost() {
                    self.session.reset().await;
                }
                // The model reads the message and can fix paths/ids itself.
                Ok(ToolOutput::error(e.to_string()))
            }
        }
    }
}

// ---------------------------------------------------------------------------- arg helpers

fn bad(msg: impl Into<String>) -> ConcatError {
    ConcatError::InvalidArgs(msg.into())
}

fn s<'a>(a: &'a Map<String, Value>, k: &str) -> Result<&'a str, ConcatError> {
    a.get(k).and_then(Value::as_str).filter(|v| !v.is_empty()).ok_or_else(|| bad(format!("`{k}` (string) is required")))
}

fn opt_s<'a>(a: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str).filter(|v| !v.is_empty())
}

fn n(a: &Map<String, Value>, k: &str) -> Result<f64, ConcatError> {
    a.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).ok_or_else(|| bad(format!("`{k}` (number) is required")))
}

fn opt_n(a: &Map<String, Value>, k: &str) -> Option<f64> {
    a.get(k).and_then(Value::as_f64).filter(|v| v.is_finite())
}

fn flag(a: &Map<String, Value>, k: &str) -> bool {
    a.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn strings(a: &Map<String, Value>, k: &str) -> Result<Vec<String>, ConcatError> {
    let arr = a.get(k).and_then(Value::as_array).ok_or_else(|| bad(format!("`{k}` (array of strings) is required")))?;
    arr.iter()
        .map(|v| v.as_str().map(str::to_owned).ok_or_else(|| bad(format!("`{k}` must contain strings"))))
        .collect()
}

fn from<T: serde::de::DeserializeOwned>(a: &Map<String, Value>, k: &str) -> Result<T, ConcatError> {
    let v = a.get(k).ok_or_else(|| bad(format!("`{k}` is required")))?;
    serde_json::from_value(v.clone()).map_err(|e| bad(format!("`{k}`: {e}")))
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<Value, ConcatError> {
    serde_json::to_value(v).map_err(|e| ConcatError::Decode { tool: "serialize".into(), reason: e.to_string() })
}

fn video_format(a: &Map<String, Value>) -> VideoSettings {
    match opt_s(a, "format") {
        Some("landscape") => VideoSettings::LANDSCAPE_1080,
        Some("square") => VideoSettings::SQUARE_1080,
        _ => VideoSettings::VERTICAL_1080,
    }
}

fn dry_run(a: &Map<String, Value>) -> Result<Value, ConcatError> {
    let plan: StudioClipPlan = from(a, "plan")?;
    let cmds = plan::to_concat_commands(&plan)?;
    Ok(json!({ "output_seconds": plan.output_duration(), "commands": cmds }))
}

// ---------------------------------------------------------------------------- dispatch

async fn dispatch(kind: Kind, c: &ConcatClient, a: &Map<String, Value>) -> Result<Value, ConcatError> {
    match kind {
        Kind::Status => {
            let v = c.version().await?;
            Ok(json!({
                "concat": v.concat, "api_version": v.api_version, "mode": v.mode(),
                "backend": format!("{:?}", c.backend().kind()).to_lowercase(),
                "capabilities": v.capabilities, "details": v.mcp,
            }))
        }
        Kind::Project => project(c, a).await,
        Kind::Media => {
            let file = s(a, "file")?;
            match s(a, "action")? {
                "probe" => c.media_probe(file).await,
                "import" => {
                    let id = c.import_media(s(a, "path")?, file).await?;
                    Ok(json!({ "media_id": id }))
                }
                o => Err(bad(format!("unknown action `{o}`"))),
            }
        }
        Kind::Cut => cut(c, a).await,
        Kind::Text => text(c, a).await,
        Kind::Keyframe => keyframe(c, a).await,
        Kind::Effect => effect(c, a).await,
        Kind::Preview => {
            let path = s(a, "path")?;
            let time = n(a, "time")?;
            let output = match opt_s(a, "output") {
                Some(o) => o.to_owned(),
                None => format!("{}/hk-preview-{}.png", path.trim_end_matches('/'), (time * 1000.0).round() as i64),
            };
            let size = match (a.get("width").and_then(Value::as_u64), a.get("height").and_then(Value::as_u64)) {
                (Some(w), Some(h)) => Some((w as u32, h as u32)),
                _ => None,
            };
            to_json(&c.get_frame(path, time, &output, size).await?)
        }
        Kind::Export => export(c, a).await,
        Kind::ApplyPlan => {
            let plan: StudioClipPlan = from(a, "plan")?;
            let applied = plan::apply_plan(c, s(a, "location")?, &plan).await?;
            Ok(json!({
                "project": applied.project,
                "range_clips": applied.range_clips,
                "timeline": to_json(&applied.summary.timeline)?,
            }))
        }
        Kind::Raw => {
            let r = c.call(s(a, "tool")?, a.get("arguments").cloned().unwrap_or_else(|| json!({}))).await?;
            Ok(r.value)
        }
    }
}

async fn project(c: &ConcatClient, a: &Map<String, Value>) -> Result<Value, ConcatError> {
    match s(a, "action")? {
        "list" => to_json(&c.project_list().await?),
        "create" => {
            let sum = c.project_create(s(a, "location")?, s(a, "name")?, Some(video_format(a))).await?;
            let path = format!("{}/{}", s(a, "location")?.trim_end_matches('/'), s(a, "name")?);
            Ok(json!({ "path": path, "summary": to_json(&sum)? }))
        }
        "open" => to_json(&c.project_open(s(a, "path")?).await?),
        "save" => {
            c.project_save(s(a, "path")?, opt_s(a, "name")).await?;
            Ok(json!({ "saved": true }))
        }
        "close" => {
            c.project_close(s(a, "path")?, flag(a, "save")).await?;
            Ok(json!({ "closed": true }))
        }
        "read" => to_json(&c.timeline_read(s(a, "path")?).await?),
        "as_plan" => to_json(&plan::plan_from_view(&c.project_get(s(a, "path")?).await?, None)?),
        o => Err(bad(format!("unknown action `{o}`"))),
    }
}

async fn cut(c: &ConcatClient, a: &Map<String, Value>) -> Result<Value, ConcatError> {
    let act = s(a, "action")?;
    let p = s(a, "path")?;
    let sum = match act {
        "add_clip" => {
            let id = c.add_clip(p, s(a, "media_id")?, s(a, "track_id")?, n(a, "start")?, flag(a, "ripple")).await?;
            return Ok(json!({ "clip_id": id }));
        }
        "add_track" => {
            let id = c.add_track(p, flag(a, "bottom")).await?;
            return Ok(json!({ "track_id": id }));
        }
        "split" => c.split(p, &strings(a, "clip_ids")?, n(a, "time")?).await?,
        "trim" => {
            let edge = match s(a, "edge")? {
                "start" => Edge::Start,
                "end" => Edge::End,
                o => return Err(bad(format!("edge must be start|end, got `{o}`"))),
            };
            c.trim(p, s(a, "clip_id")?, edge, n(a, "delta")?, flag(a, "ripple")).await?
        }
        "delete" => c.remove_clips(p, &strings(a, "clip_ids")?, flag(a, "ripple")).await?,
        "move" => {
            #[derive(serde::Deserialize)]
            struct M {
                clip_id: String,
                start: f64,
                track_id: String,
            }
            let ms: Vec<M> = from(a, "moves")?;
            let moves: Vec<ClipMove> =
                ms.into_iter().map(|m| ClipMove { clip_id: m.clip_id, start: m.start, track_id: m.track_id }).collect();
            c.move_clips(p, &moves).await?
        }
        "cut_range" => c.cut_range(p, n(a, "from")?, n(a, "to")?).await?,
        "speed" => c.clip_speed(p, s(a, "clip_id")?, n(a, "speed")?).await?,
        "undo" => c.undo(p).await?,
        "redo" => c.redo(p).await?,
        o => return Err(bad(format!("unknown action `{o}`"))),
    };
    to_json(&sum.timeline)
}

async fn text(c: &ConcatClient, a: &Map<String, Value>) -> Result<Value, ConcatError> {
    let p = s(a, "path")?;
    match s(a, "action")? {
        "title" => {
            let id = c
                .add_title(p, s(a, "text")?, n(a, "start")?, n(a, "duration")?, a.get("style").cloned(), opt_n(a, "offset_y"))
                .await?;
            Ok(json!({ "clip_id": id }))
        }
        "captions" => {
            let lines: Vec<CaptionLine> = from(a, "lines")?;
            let sum = c.add_captions(p, &lines, a.get("style").cloned(), opt_n(a, "offset_y")).await?;
            Ok(json!({ "added": lines.len(), "duration": sum.timeline.duration }))
        }
        "captions_from_words" => {
            let words: Vec<plan::Word> = from(a, "words")?;
            let mut style: CaptionStyle = a
                .get("style")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()
                .map_err(|e| bad(format!("`style`: {e}")))?
                .unwrap_or_default();
            if let Some(v) = a.get("max_words_per_line").and_then(Value::as_u64) {
                style.max_words_per_line = v as usize;
            }
            if let Some(v) = a.get("max_chars_per_line").and_then(Value::as_u64) {
                style.max_chars_per_line = v as usize;
            }
            style.uppercase |= flag(a, "uppercase");
            if let Some(y) = opt_n(a, "offset_y") {
                style.offset_y = Some(y);
            }
            let timed: Vec<TimedWord> = words
                .into_iter()
                .filter(|w| !w.text.trim().is_empty() && w.end >= w.start)
                .map(|w| TimedWord { text: w.text.trim().to_owned(), start: w.start, end: w.end, range: 0 })
                .collect();
            let lines = plan::group_words(&timed, &style);
            let sum = c.add_captions(p, &lines, Some(style.text_style()), Some(style.offset())).await?;
            Ok(json!({ "added": lines.len(), "duration": sum.timeline.duration }))
        }
        o => Err(bad(format!("unknown action `{o}`"))),
    }
}

async fn keyframe(c: &ConcatClient, a: &Map<String, Value>) -> Result<Value, ConcatError> {
    let p = s(a, "path")?;
    let clip = s(a, "clip_id")?;
    let prop = |a: &Map<String, Value>| {
        let name = s(a, "property")?;
        KeyProperty::parse(name).ok_or_else(|| bad(format!("unknown property `{name}`")))
    };
    let sum = match s(a, "action")? {
        "set" => {
            let ease = match a.get("ease") {
                Some(v) => Some(serde_json::from_value::<[f64; 4]>(v.clone()).map_err(|e| bad(format!("`ease`: {e}")))?),
                None => None,
            };
            c.set_keyframe(p, clip, prop(a)?, n(a, "at")?, n(a, "value")?, ease).await?
        }
        "clear" => c.clear_keyframe(p, clip, prop(a)?, n(a, "at")?).await?,
        "transform" => {
            c.set_transform(p, clip, opt_n(a, "scale"), opt_n(a, "offset_x"), opt_n(a, "offset_y"), opt_n(a, "rotation")).await?
        }
        "frame" => {
            let f = if s(a, "framing")? == "fit" { Framing::Fit } else { Framing::Fill };
            c.frame_clip(p, clip, f, opt_n(a, "at").unwrap_or(0.0)).await?
        }
        "reframe" => return reframe(c, p, clip, a).await,
        o => return Err(bad(format!("unknown action `{o}`"))),
    };
    to_json(&sum.timeline)
}

async fn reframe(c: &ConcatClient, p: &str, clip_id: &str, a: &Map<String, Value>) -> Result<Value, ConcatError> {
    #[derive(serde::Deserialize)]
    struct P {
        at: f64,
        center_x: f64,
        center_y: f64,
        #[serde(default = "one")]
        zoom: f64,
    }
    fn one() -> f64 {
        1.0
    }
    let points: Vec<P> = from(a, "points")?;
    if points.is_empty() {
        return Err(bad("`points` is empty"));
    }
    let s0 = c.timeline_read(p).await?;
    let clip = s0.clip(clip_id).ok_or_else(|| bad(format!("no clip `{clip_id}`; read ids with concat_project read")))?;
    let media = clip
        .media
        .as_deref()
        .and_then(|m| s0.media_item(m))
        .ok_or_else(|| bad("clip has no media (only video/image clips can be reframed)"))?;
    let size = (f64::from(media.width.ok_or_else(|| bad("media size unknown"))?), f64::from(media.height.ok_or_else(|| bad("media size unknown"))?));
    let frame = (f64::from(s0.project.width), f64::from(s0.project.height));
    let mut ops = Vec::new();
    for pt in &points {
        let (sc, ox, oy) = plan::crop_to_transform(size, frame, pt.center_x, pt.center_y, pt.zoom)
            .ok_or_else(|| bad("cannot compute reframe from zero sizes"))?;
        for (prop, v) in [("scale", sc), ("offsetX", ox), ("offsetY", oy)] {
            ops.push(json!({ "op": "setClipKey", "clipId": clip_id, "property": prop, "at": pt.at.clamp(0.0, 1.0), "value": v }));
        }
    }
    let sum = c.edit_apply(p, json!({ "op": "batch", "commands": ops })).await?;
    to_json(&sum.timeline)
}

async fn effect(c: &ConcatClient, a: &Map<String, Value>) -> Result<Value, ConcatError> {
    match s(a, "action")? {
        "list" => c.effects_list(opt_s(a, "kind")).await,
        "set_effects" => {
            let chain = a.get("chain").cloned().ok_or_else(|| bad("`chain` is required"))?;
            to_json(&c.set_effects(s(a, "path")?, s(a, "clip_id")?, chain).await?.timeline)
        }
        "transition" => {
            let t = match opt_s(a, "id") {
                Some(id) => Some((id, opt_n(a, "duration").unwrap_or(0.5))),
                None => None,
            };
            to_json(&c.set_transition(s(a, "path")?, s(a, "clip_id")?, t).await?.timeline)
        }
        "layer" => {
            let id = c.add_effect_layer(s(a, "path")?, s(a, "effect_id")?, s(a, "name")?, n(a, "start")?, n(a, "duration")?).await?;
            Ok(json!({ "clip_id": id }))
        }
        o => Err(bad(format!("unknown action `{o}`"))),
    }
}

async fn export(c: &ConcatClient, a: &Map<String, Value>) -> Result<Value, ConcatError> {
    match s(a, "action")? {
        "start" => {
            let preset = match opt_s(a, "preset").unwrap_or("vertical_1080") {
                "draft" => ExportPreset::Draft,
                "vertical_1080" => ExportPreset::Vertical1080,
                "landscape_1080" => ExportPreset::Landscape1080,
                "master" => ExportPreset::Master,
                o => return Err(bad(format!("unknown preset `{o}`"))),
            };
            let mut spec = preset.spec(s(a, "output")?);
            if let Some(crf) = a.get("crf").and_then(Value::as_u64) {
                spec.crf = Some(crf.min(51) as u8);
            }
            if let Some(codec) = opt_s(a, "codec") {
                spec.codec = Some(codec.to_owned());
            }
            to_json(&c.export_start(s(a, "path")?, &spec).await?)
        }
        "status" => to_json(&c.export_status(s(a, "job")?).await?),
        "wait" => {
            let secs = a.get("timeout_secs").and_then(Value::as_u64).unwrap_or(DEFAULT_WAIT_SECS).clamp(1, MAX_WAIT_SECS);
            match c.export_wait(s(a, "job")?, Duration::from_millis(500), Duration::from_secs(secs), |_| {}).await {
                Ok(st) => to_json(&st),
                // Still running is not a failure: the job lives on, the model just calls again.
                Err(ConcatError::Timeout(_)) => {
                    let mut v = to_json(&c.export_status(s(a, "job")?).await?)?;
                    v["note"] = json!("still running: call wait again, or cancel");
                    Ok(v)
                }
                Err(e) => Err(e),
            }
        }
        "cancel" => {
            c.export_cancel(s(a, "job")?).await?;
            Ok(json!({ "cancelled": true }))
        }
        o => Err(bad(format!("unknown action `{o}`"))),
    }
}
