use std::time::SystemTime;

use async_trait::async_trait;
use globset::Glob;
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{base_dir, fail, join_err, parse};
use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};

const MAX_RESULTS: usize = 100;

#[derive(Debug)]
pub struct GlobTool;

#[derive(Deserialize)]
struct Args {
    pattern: String,
    path: Option<String>,
}

#[async_trait]
impl Tool for GlobTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "glob".into(),
            description: "Find files by glob pattern (e.g. `**/*.rs`). Respects .gitignore. Newest first, max 100.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string", "description": "Directory to search (default: working directory)."}
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = parse("glob", args)?;
        let root = base_dir(&ctx, a.path.as_deref());
        let matcher = match Glob::new(&a.pattern) {
            Ok(g) => g.compile_matcher(),
            Err(e) => return fail(format!("bad glob: {e}")),
        };
        let found = tokio::task::spawn_blocking(move || {
            let mut hits: Vec<(SystemTime, String)> = Vec::new();
            for e in WalkBuilder::new(&root).build().flatten() {
                if !e.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let rel = e.path().strip_prefix(&root).unwrap_or(e.path());
                if matcher.is_match(rel) {
                    let mtime = e
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    hits.push((mtime, e.path().display().to_string()));
                }
            }
            hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            hits
        })
        .await
        .map_err(|e| join_err("glob", e))?;
        if found.is_empty() {
            return Ok(ToolOutput::ok("no files matched"));
        }
        let total = found.len();
        let mut out: Vec<String> = found.into_iter().take(MAX_RESULTS).map(|h| h.1).collect();
        if total > MAX_RESULTS {
            out.push(format!("[{} more not shown]", total - MAX_RESULTS));
        }
        Ok(ToolOutput::ok(out.join("\n")))
    }
}
