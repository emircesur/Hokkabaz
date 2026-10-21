use async_trait::async_trait;
use globset::Glob;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{base_dir, fail, join_err, looks_binary, parse};
use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};

const MAX_MATCHES: usize = 200;
const MAX_LINE: usize = 500;

#[derive(Debug)]
pub struct GrepTool;

#[derive(Deserialize)]
struct Args {
    pattern: String,
    path: Option<String>,
    /// Only search files whose relative path matches this glob.
    include: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
}

#[async_trait]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Search file contents with a regex. Respects .gitignore. Returns `path:line:text`, max 200 matches.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Rust regex."},
                    "path": {"type": "string", "description": "File or directory (default: working directory)."},
                    "include": {"type": "string", "description": "Glob filter on file paths, e.g. `*.rs`."},
                    "case_insensitive": {"type": "boolean"}
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = parse("grep", args)?;
        let root = base_dir(&ctx, a.path.as_deref());
        let re = match RegexBuilder::new(&a.pattern)
            .case_insensitive(a.case_insensitive)
            .build()
        {
            Ok(r) => r,
            Err(e) => return fail(format!("bad regex: {e}")),
        };
        let include = match a.include.as_deref().map(Glob::new).transpose() {
            Ok(g) => g.map(|g| g.compile_matcher()),
            Err(e) => return fail(format!("bad include glob: {e}")),
        };
        let (lines, truncated) = tokio::task::spawn_blocking(move || {
            let mut lines = Vec::new();
            for e in WalkBuilder::new(&root).build().flatten() {
                if !e.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let rel = e.path().strip_prefix(&root).unwrap_or(e.path());
                if let Some(m) = &include
                    && !m.is_match(rel)
                    && !e.path().file_name().is_some_and(|n| m.is_match(n))
                {
                    continue;
                }
                let Ok(bytes) = std::fs::read(e.path()) else {
                    continue;
                };
                if looks_binary(&bytes) {
                    continue;
                }
                let text = String::from_utf8_lossy(&bytes);
                for (i, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        if lines.len() == MAX_MATCHES {
                            return (lines, true);
                        }
                        let mut cut = line.len().min(MAX_LINE);
                        while !line.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        lines.push(format!("{}:{}:{}", e.path().display(), i + 1, &line[..cut]));
                    }
                }
            }
            (lines, false)
        })
        .await
        .map_err(|e| join_err("grep", e))?;
        if lines.is_empty() {
            return Ok(ToolOutput::ok("no matches"));
        }
        let mut out = lines.join("\n");
        if truncated {
            out.push_str(&format!(
                "\n[stopped at {MAX_MATCHES} matches; narrow the search]"
            ));
        }
        Ok(ToolOutput::ok(out))
    }
}
