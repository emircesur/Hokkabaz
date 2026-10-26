//! `studio_*` agent tools over [`StudioService`]: the same operations the Studio
//! screen performs, so "make five viral shorts from this video" is something the
//! agent can do while the user watches the editor update live.
// port: none (Hokkabaz-native; see docs/STUDIO.md)

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hk_tools::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};
use serde_json::{Map, Value, json};

use crate::broll::BrollOpts;
use crate::edit::EditOp;
use crate::highlights::HighlightOpts;
use crate::live::StudioEvent;
use crate::pipeline::preview;
use crate::project::StudioProject;
use crate::service::StudioService;
use crate::social::{Platform, SocialOpts};

const MAX_WORDS_SHOWN: usize = 1500;
const MAX_RENDER_WAIT_SECS: u64 = 1800;

/// All Studio tools, for `AgentRuntime::add_tools`.
pub fn studio_tools(svc: Arc<StudioService>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(Projects(svc.clone())),
        Arc::new(Transcribe(svc.clone())),
        Arc::new(Transcript(svc.clone())),
        Arc::new(Highlights(svc.clone())),
        Arc::new(Edit(svc.clone())),
        Arc::new(Track(svc.clone())),
        Arc::new(Broll(svc.clone())),
        Arc::new(Social(svc.clone())),
        Arc::new(Render(svc.clone())),
        Arc::new(Export(svc.clone())),
        Arc::new(Publish(svc)),
    ]
}

fn obj<'a>(tool: &str, args: &'a Value) -> Result<&'a Map<String, Value>, ToolError> {
    args.as_object()
        .ok_or_else(|| bad(tool, "arguments must be an object"))
}

fn bad(tool: &str, reason: impl Into<String>) -> ToolError {
    ToolError::InvalidArgs {
        tool: tool.into(),
        reason: reason.into(),
    }
}

fn failed(tool: &str, e: impl std::fmt::Display) -> ToolError {
    ToolError::Failed {
        tool: tool.into(),
        reason: e.to_string(),
    }
}

fn s<'a>(tool: &str, a: &'a Map<String, Value>, key: &str) -> Result<&'a str, ToolError> {
    a.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| bad(tool, format!("`{key}` is required")))
}

fn f(a: &Map<String, Value>, key: &str) -> Option<f64> {
    a.get(key).and_then(Value::as_f64)
}

fn out(v: &Value) -> Result<ToolOutput, ToolError> {
    Ok(ToolOutput::ok(v.to_string()))
}

fn r1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn clip_json(p: &StudioProject, c: &crate::project::Clip) -> Value {
    let plan = preview(p, &c.id).ok();
    json!({
        "id": c.id, "title": c.title, "start": r1(c.start), "end": r1(c.end),
        "duration_after_cuts": plan.as_ref().map(|x| r1(x.duration)),
        "deleted_words": c.deleted_words.len(), "remove_fillers": c.remove_fillers,
        "template": c.template, "has_captions": c.captions.is_some(),
        "reframe": c.reframe.as_ref().map(|r| json!({"aspect": r.aspect, "mode": r.mode, "tracked": !r.focus.is_empty()})),
        "broll": c.broll.len(),
        "score": c.score.as_ref().map(|x| json!({"total": x.total, "summary": x.summary, "improvements": x.improvements})),
        "hook": c.hook, "reason": c.reason,
        "social_platforms": c.social.as_ref().map(|x| x.posts.keys().cloned().collect::<Vec<_>>()),
    })
}

fn project_json(p: &StudioProject) -> Value {
    json!({
        "id": p.id, "name": p.name,
        "source": {"duration": r1(p.source.duration), "width": p.source.width, "height": p.source.height, "has_audio": p.source.has_audio},
        "transcribed": p.transcript.is_some(),
        "words": p.transcript.as_ref().map(|t| t.words.len()),
        "clips": p.clips.iter().map(|c| clip_json(p, c)).collect::<Vec<_>>(),
    })
}

// --------------------------------------------------------------- projects

struct Projects(Arc<StudioService>);

#[async_trait]
impl Tool for Projects {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "studio_projects".into(),
            description: "Video Studio projects. action=list; get (full state incl. clips, scores, edits); import (a video file path, creates a project); delete.".into(),
            parameters: json!({"type":"object","properties":{
                "action":{"type":"string","enum":["list","get","import","delete"]},
                "project":{"type":"string","description":"Project id (get, delete)."},
                "path":{"type":"string","description":"Video file path (import)."},
                "name":{"type":"string","description":"Project name (import)."}},
                "required":["action"],"additionalProperties":false}),
        }
    }

    fn risk(&self, args: &Value) -> Risk {
        match args.get("action").and_then(Value::as_str) {
            Some("import" | "delete") => Risk::Ask,
            _ => Risk::Safe,
        }
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        const T: &str = "studio_projects";
        let a = obj(T, &args)?;
        match a.get("action").and_then(Value::as_str) {
            Some("list") => out(&json!(self.0.projects().await.map_err(|e| failed(T, e))?)),
            Some("get") => {
                let live = self
                    .0
                    .open(s(T, a, "project")?)
                    .await
                    .map_err(|e| failed(T, e))?;
                out(&project_json(&live.snapshot().await))
            }
            Some("import") => {
                let live = self
                    .0
                    .import(
                        Path::new(s(T, a, "path")?),
                        a.get("name").and_then(Value::as_str),
                    )
                    .await
                    .map_err(|e| failed(T, e))?;
                out(&project_json(&live.snapshot().await))
            }
            Some("delete") => {
                self.0
                    .delete(s(T, a, "project")?)
                    .await
                    .map_err(|e| failed(T, e))?;
                out(&json!({"deleted": true}))
            }
            _ => Err(bad(T, "`action` must be list, get, import or delete")),
        }
    }
}

// ------------------------------------------------------------- transcribe

struct Transcribe(Arc<StudioService>);

#[async_trait]
impl Tool for Transcribe {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "studio_transcribe".into(),
            description: "Transcribe the project's audio with word-level timestamps (needed before highlights or text editing). Sends audio to the configured speech-to-text engine.".into(),
            parameters: json!({"type":"object","properties":{"project":{"type":"string"}},"required":["project"],"additionalProperties":false}),
        }
    }

    fn risk(&self, _args: &Value) -> Risk {
        Risk::Ask
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        const T: &str = "studio_transcribe";
        let id = s(T, obj(T, &args)?, "project")?;
        self.0.transcribe(id).await.map_err(|e| failed(T, e))?;
        let p = self
            .0
            .open(id)
            .await
            .map_err(|e| failed(T, e))?
            .snapshot()
            .await;
        let t = p.transcript.as_ref();
        out(
            &json!({"words": t.map(|t| t.words.len()), "language": t.and_then(|t| t.language.clone()), "fillers": t.map(|t| t.filler_indices().len())}),
        )
    }
}

struct Transcript(Arc<StudioService>);

#[async_trait]
impl Tool for Transcript {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "studio_transcript".into(),
            description: "Read transcript words with their indices (for text-based editing with studio_edit delete_words). Optional time range in source seconds; fillers are marked with *.".into(),
            parameters: json!({"type":"object","properties":{
                "project":{"type":"string"},"start":{"type":"number"},"end":{"type":"number"}},
                "required":["project"],"additionalProperties":false}),
        }
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        const T: &str = "studio_transcript";
        let a = obj(T, &args)?;
        let p = self
            .0
            .open(s(T, a, "project")?)
            .await
            .map_err(|e| failed(T, e))?
            .snapshot()
            .await;
        let t = p.transcript().map_err(|e| failed(T, e))?;
        let (start, end) = (
            f(a, "start").unwrap_or(0.0),
            f(a, "end").unwrap_or(f64::MAX),
        );
        let mut lines = Vec::new();
        for (shown, (i, w)) in t
            .words
            .iter()
            .enumerate()
            .filter(|(_, w)| w.end > start && w.start < end)
            .enumerate()
        {
            if shown >= MAX_WORDS_SHOWN {
                lines.push(format!(
                    "... truncated at {MAX_WORDS_SHOWN} words; pass start/end to read more"
                ));
                break;
            }
            lines.push(format!(
                "{i}|{:.1}|{}{}",
                w.start,
                w.text,
                if w.flags.filler { "*" } else { "" }
            ));
        }
        Ok(ToolOutput::ok(format!(
            "index|start|word\n{}",
            lines.join("\n")
        )))
    }
}

// ------------------------------------------------------------- highlights

struct Highlights(Arc<StudioService>);

#[async_trait]
impl Tool for Highlights {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "studio_highlights".into(),
            description: "Find the most engaging moments (funny, insightful, hooky) of the transcribed video and add them as scored clips, best first. Optionally apply a template (shorts-karaoke, shorts-pop, clean-vertical, square-minimal, landscape-captions).".into(),
            parameters: json!({"type":"object","properties":{
                "project":{"type":"string"},
                "count":{"type":"integer","minimum":1,"maximum":30,"description":"How many clips (default 8)."},
                "min_secs":{"type":"number"},"max_secs":{"type":"number"},
                "brief":{"type":"string","description":"Editor's brief, e.g. 'only funny moments' or 'about pricing'."},
                "rubric":{"type":"string","description":"Extra virality criteria."},
                "template":{"type":"string"}},
                "required":["project"],"additionalProperties":false}),
        }
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        const T: &str = "studio_highlights";
        let a = obj(T, &args)?;
        let id = s(T, a, "project")?;
        let mut o = HighlightOpts::default();
        if let Some(n) = a.get("count").and_then(Value::as_u64) {
            o.count = n.clamp(1, 30) as usize;
        }
        o.min_secs = f(a, "min_secs").unwrap_or(o.min_secs);
        o.max_secs = f(a, "max_secs").unwrap_or(o.max_secs);
        o.brief = a.get("brief").and_then(Value::as_str).map(str::to_string);
        let ids = self
            .0
            .analyze(
                id,
                &o,
                a.get("rubric").and_then(Value::as_str),
                a.get("template").and_then(Value::as_str),
            )
            .await
            .map_err(|e| failed(T, e))?;
        let p = self
            .0
            .open(id)
            .await
            .map_err(|e| failed(T, e))?
            .snapshot()
            .await;
        let clips: Vec<Value> = p
            .clips
            .iter()
            .filter(|c| ids.contains(&c.id))
            .map(|c| clip_json(&p, c))
            .collect();
        out(&json!({"added": clips.len(), "clips": clips}))
    }
}

// ------------------------------------------------------------------- edit

struct Edit(Arc<StudioService>);

#[async_trait]
impl Tool for Edit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "studio_edit".into(),
            description: "Edit clips. `ops` is a list of operations, each {\"op\": ...}: add_clip{title,start,end}, remove_clip{clip}, rename_clip{clip,title}, set_bounds{clip,start,end}, split_clip{clip,at}, delete_words{clip,words[]}, restore_words{clip,words[]}, delete_range{clip,start,end}, set_fillers{clip,on}, set_silence{clip,silence:{min_gap,keep}|null}, set_caption_preset{clip,preset: classic|karaoke|pop|minimal}, set_captions{clip,style}, set_reframe{clip,reframe}, remove_broll{clip,index}, set_music{clip,music}, apply_template{clip,template}. Times are source seconds; word indices come from studio_transcript. The user sees changes live in the editor.".into(),
            parameters: json!({"type":"object","properties":{
                "project":{"type":"string"},
                "ops":{"type":"array","items":{"type":"object","properties":{"op":{"type":"string"}},"required":["op"]},"minItems":1,"maxItems":50}},
                "required":["project","ops"],"additionalProperties":false}),
        }
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        const T: &str = "studio_edit";
        let a = obj(T, &args)?;
        let id = s(T, a, "project")?;
        let ops: Vec<EditOp> = serde_json::from_value(a.get("ops").cloned().unwrap_or(Value::Null))
            .map_err(|e| bad(T, format!("bad ops: {e}")))?;
        let live = self.0.open(id).await.map_err(|e| failed(T, e))?;
        for (i, op) in ops.into_iter().enumerate() {
            live.edit(op, "agent")
                .await
                .map_err(|e| failed(T, format!("op #{i}: {e}")))?;
        }
        out(&project_json(&live.snapshot().await))
    }
}

// ------------------------------------------------------------ track / b-roll / social

struct Track(Arc<StudioService>);

#[async_trait]
impl Tool for Track {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "studio_reframe".into(),
            description: "Smart reframing: follow the speaker's face (or the moving subject) and crop the clip to a vertical 9:16 frame (aspect=square for 1:1). Re-run after changing the clip bounds.".into(),
            parameters: json!({"type":"object","properties":{
                "project":{"type":"string"},"clip":{"type":"string"},
                "aspect":{"type":"string","enum":["vertical","square"]}},
                "required":["project","clip"],"additionalProperties":false}),
        }
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        const T: &str = "studio_reframe";
        let a = obj(T, &args)?;
        let base = match a.get("aspect").and_then(Value::as_str) {
            Some("square") => Some(crate::reframe::ReframeSpec::square()),
            _ => Some(crate::reframe::ReframeSpec::vertical()),
        };
        let n = self
            .0
            .track(s(T, a, "project")?, s(T, a, "clip")?, base)
            .await
            .map_err(|e| failed(T, e))?;
        out(&json!({"tracked_points": n}))
    }
}

struct Broll(Arc<StudioService>);

#[async_trait]
impl Tool for Broll {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "studio_broll".into(),
            description: "Insert stock B-roll (images / video) at moments where it illustrates what is said. Needs a stock source (Pexels, Pixabay or a local folder) in Studio settings.".into(),
            parameters: json!({"type":"object","properties":{
                "project":{"type":"string"},"clip":{"type":"string"},"max":{"type":"integer","minimum":1,"maximum":10}},
                "required":["project","clip"],"additionalProperties":false}),
        }
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        const T: &str = "studio_broll";
        let a = obj(T, &args)?;
        let mut o = BrollOpts::default();
        if let Some(m) = a.get("max").and_then(Value::as_u64) {
            o.max = m.clamp(1, 10) as usize;
        }
        let n = self
            .0
            .add_broll(s(T, a, "project")?, s(T, a, "clip")?, &o)
            .await
            .map_err(|e| failed(T, e))?;
        out(&json!({"inserted": n}))
    }
}

struct Social(Arc<StudioService>);

#[async_trait]
impl Tool for Social {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "studio_social".into(),
            description:
                "Write titles, descriptions and hashtags for a clip, within each platform's limits."
                    .into(),
            parameters: json!({"type":"object","properties":{
                "project":{"type":"string"},"clip":{"type":"string"},
                "platforms":{"type":"array","items":{"type":"string","enum":["youtube_shorts","tiktok","instagram_reels","x","linkedin"]}},
                "tone":{"type":"string"},"language":{"type":"string"},"call_to_action":{"type":"string"}},
                "required":["project","clip"],"additionalProperties":false}),
        }
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        const T: &str = "studio_social";
        let a = obj(T, &args)?;
        let (id, clip) = (s(T, a, "project")?, s(T, a, "clip")?);
        let platforms: Vec<Platform> = match a.get("platforms") {
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| bad(T, format!("bad platforms: {e}")))?,
            None => Platform::ALL.to_vec(),
        };
        let opts = SocialOpts {
            tone: a.get("tone").and_then(Value::as_str).map(str::to_string),
            language: a
                .get("language")
                .and_then(Value::as_str)
                .map(str::to_string),
            call_to_action: a
                .get("call_to_action")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        self.0
            .social(id, clip, &platforms, &opts)
            .await
            .map_err(|e| failed(T, e))?;
        let p = self
            .0
            .open(id)
            .await
            .map_err(|e| failed(T, e))?
            .snapshot()
            .await;
        out(&json!(p.clip(clip).map_err(|e| failed(T, e))?.social))
    }
}

// ------------------------------------------------------- render / export

struct Render(Arc<StudioService>);

#[async_trait]
impl Tool for Render {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "studio_render".into(),
            description: "Render a clip to an MP4 with cuts, captions, reframing and B-roll applied. Waits for the result by default.".into(),
            parameters: json!({"type":"object","properties":{
                "project":{"type":"string"},"clip":{"type":"string"},
                "wait":{"type":"boolean","description":"Wait until done (default true)."},
                "audio_cleanup":{"type":"string","enum":["none","podcast","social"],"description":"Voice cleanup + loudness preset."},
                "encoder":{"type":"string","enum":["auto","software","nvenc","qsv","vaapi","videotoolbox","mediacodec"],"description":"Video encoder family (default software)."},
                "timeout_secs":{"type":"integer","minimum":10,"maximum":1800}},
                "required":["project","clip"],"additionalProperties":false}),
        }
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        const T: &str = "studio_render";
        let a = obj(T, &args)?;
        let (id, clip) = (s(T, a, "project")?, s(T, a, "clip")?);
        let live = self.0.open(id).await.map_err(|e| failed(T, e))?;
        let mut rx = live.subscribe();
        let job = self
            .0
            .start_render(id, clip, self.0.render_opts(&args).await)
            .await
            .map_err(|e| failed(T, e))?;
        if a.get("wait").and_then(Value::as_bool) == Some(false) {
            return out(&json!({"job": job, "status": "started"}));
        }
        let secs = a
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(600)
            .min(MAX_RENDER_WAIT_SECS);
        let wait = async {
            loop {
                match rx.recv().await {
                    Ok(StudioEvent::RenderDone { clip: c, path }) if c == clip => return Ok(path),
                    Ok(StudioEvent::RenderFailed { clip: c, error }) if c == clip => {
                        return Err(error);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err("session closed".to_string());
                    }
                    _ => {}
                }
            }
        };
        match tokio::time::timeout(Duration::from_secs(secs), wait).await {
            Ok(Ok(path)) => out(&json!({"job": job, "status": "done", "path": path})),
            Ok(Err(e)) => Ok(ToolOutput::error(format!("render failed: {e}"))),
            Err(_) => out(
                &json!({"job": job, "status": "running", "note": "still rendering; call studio_export later"}),
            ),
        }
    }
}

struct Export(Arc<StudioService>);

#[async_trait]
impl Tool for Export {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "studio_export".into(),
            description: "Write a ready-to-post package (clip.mp4, captions.srt/.vtt, one text file per platform) of a rendered clip into a folder.".into(),
            parameters: json!({"type":"object","properties":{
                "project":{"type":"string"},"clip":{"type":"string"},
                "dir":{"type":"string","description":"Destination folder."}},
                "required":["project","clip","dir"],"additionalProperties":false}),
        }
    }

    fn risk(&self, _args: &Value) -> Risk {
        Risk::Ask
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        const T: &str = "studio_export";
        let a = obj(T, &args)?;
        let p = self
            .0
            .export_package(
                s(T, a, "project")?,
                s(T, a, "clip")?,
                Path::new(s(T, a, "dir")?),
            )
            .await
            .map_err(|e| failed(T, e))?;
        out(&json!({"folder": p.to_string_lossy()}))
    }
}

struct Publish(Arc<StudioService>);

#[async_trait]
impl Tool for Publish {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "studio_publish".into(),
            description: "Upload a rendered clip to YouTube (as a Short) using its post copy. Defaults to private so the user can review it on YouTube first. Needs YouTube set up in Studio settings.".into(),
            parameters: json!({"type":"object","properties":{
                "project":{"type":"string"},"clip":{"type":"string"},
                "privacy":{"type":"string","enum":["private","unlisted","public"]}},
                "required":["project","clip"],"additionalProperties":false}),
        }
    }

    fn risk(&self, _args: &Value) -> Risk {
        Risk::Ask
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        const T: &str = "studio_publish";
        let a = obj(T, &args)?;
        let privacy = match a.get("privacy").and_then(Value::as_str) {
            Some("public") => crate::publish::Privacy::Public,
            Some("unlisted") => crate::publish::Privacy::Unlisted,
            _ => crate::publish::Privacy::Private,
        };
        let out = self
            .0
            .publish_youtube(s(T, a, "project")?, s(T, a, "clip")?, privacy)
            .await
            .map_err(|e| failed(T, e))?;
        out_json(
            &json!({"id": out.id, "url": out.url, "privacy": format!("{privacy:?}").to_lowercase()}),
        )
    }
}

fn out_json(v: &Value) -> Result<ToolOutput, ToolError> {
    out(v)
}
