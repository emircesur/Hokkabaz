use std::sync::Arc;

use async_trait::async_trait;
use hk_tools::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};
use serde_json::{Value, json};

use crate::Studio;

const OPERATIONS: &[(&str, &str, &str)] = &[
    (
        "score_clip",
        "score_clip",
        "Re-score a candidate with a custom rubric; project_id, clip_id, model and rubric.",
    ),
    (
        "studio_projects",
        "projects",
        "List imported Studio video projects.",
    ),
    (
        "studio_import",
        "import",
        "Import a video file under the session workspace; path is required.",
    ),
    (
        "media_probe",
        "get",
        "Read a Studio project's source metadata, transcript, edits and candidates; project_id is required.",
    ),
    (
        "transcribe",
        "transcribe",
        "Transcribe a project's audio with its configured Whisper/API connection. This may upload audio; project_id is required.",
    ),
    (
        "find_highlights",
        "analyze",
        "Find and score short clips from the transcript; project_id, model (provider/model), count, min_seconds, max_seconds and rubric.",
    ),
    (
        "reframe",
        "reframe",
        "Plan subject tracking with the configured vision endpoint, or center crop without one; project_id is required.",
    ),
    (
        "caption",
        "template",
        "Apply caption template karaoke, bold_pop, podcast or minimal; project_id and template are required.",
    ),
    (
        "studio_edit",
        "edit",
        "Apply undoable edits; project_id, current revision and the complete edit object from media_probe are required.",
    ),
    (
        "add_broll",
        "add_broll",
        "Add local or stock B-roll; project_id, path or url, span {start,end} on the edited timeline, attribution.",
    ),
    (
        "studio_auto_broll",
        "auto_broll",
        "Find relevant stock B-roll and add it automatically; project_id, model and provider pexels/pixabay.",
    ),
    (
        "render",
        "render",
        "Start a vertical/square/landscape render with edited captions, cuts, reframing and overlays; project_id, optional clip_id.",
    ),
    (
        "studio_jobs",
        "jobs",
        "Inspect render progress and output paths.",
    ),
    (
        "write_social",
        "social",
        "Generate editable social metadata; project_id, optional clip_id, platform youtube/tiktok/instagram/linkedin, model.",
    ),
    (
        "studio_export",
        "export_package",
        "Export subtitle files, project, Concat plan, social metadata and optional completed video; project_id, optional clip_id/job_id.",
    ),
    (
        "studio_concat",
        "open_concat",
        "Save a rich edit project and open it in Concat; project_id, optional clip_id.",
    ),
    (
        "publish",
        "publish",
        "Upload a reviewed render to YouTube. Requires explicit user approval; project_id, job_id, confirmed:true, privacy private/unlisted/public.",
    ),
];

pub fn studio_tools(studio: Arc<Studio>) -> Vec<Arc<dyn Tool>> {
    OPERATIONS
        .iter()
        .map(|&(name, operation, description)| {
            Arc::new(StudioTool {
                studio: studio.clone(),
                name,
                operation,
                description,
            }) as Arc<dyn Tool>
        })
        .collect()
}

#[derive(Debug)]
struct StudioTool {
    studio: Arc<Studio>,
    name: &'static str,
    operation: &'static str,
    description: &'static str,
}

#[async_trait]
impl Tool for StudioTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: self.description.into(),
            parameters: json!({"type":"object","additionalProperties":true,"properties":{
                "project_id":{"type":"string"},"path":{"type":"string"},"model":{"type":"string"},
                "clip_id":{"type":"string"},"job_id":{"type":"string"},"count":{"type":"integer"},
                "min_seconds":{"type":"number"},"max_seconds":{"type":"number"},"rubric":{"type":"string"},
                "template":{"type":"string"},"revision":{"type":"integer"},"edit":{"type":"object"},
                "url":{"type":"string"},"span":{"type":"object"},"attribution":{"type":"string"},
                "provider":{"type":"string"},"platform":{"type":"string"},"confirmed":{"type":"boolean"},"privacy":{"type":"string"}
            }}),
        }
    }

    fn risk(&self, _: &Value) -> Risk {
        match self.operation {
            "projects" | "get" | "jobs" => Risk::Safe,
            "publish" => Risk::Dangerous,
            _ => Risk::Ask,
        }
    }

    async fn execute(
        &self,
        ctx: ToolCtx,
        args: Value,
    ) -> std::result::Result<ToolOutput, ToolError> {
        for path in [
            args.get("path"),
            args.get("edit").and_then(|e| e.get("music")),
        ]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        {
            let allowed = ctx.cwd.as_ref().and_then(|p| std::fs::canonicalize(p).ok());
            let actual = std::fs::canonicalize(path).ok();
            if !matches!((allowed,actual),(Some(root),Some(path)) if path.starts_with(&root)) {
                return Err(ToolError::Rejected { tool:self.name.into(),reason:"media paths must remain under the session workspace; import other files through Studio".into() });
            }
        }
        self.studio
            .request(self.operation, args)
            .await
            .map(|value| ToolOutput::ok(value.to_string()))
            .map_err(|e| ToolError::Failed {
                tool: self.name.into(),
                reason: e.to_string(),
            })
    }
}
