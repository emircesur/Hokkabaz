use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{fail, looks_binary, parse, resolve};
use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};

const DEFAULT_LIMIT: usize = 2000;
const MAX_LINE: usize = 2000;

#[derive(Debug)]
pub struct ReadTool;

#[derive(Deserialize)]
struct Args {
    path: String,
    /// 1-based first line.
    offset: Option<usize>,
    limit: Option<usize>,
}

#[async_trait]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "Read a text file with line numbers, or list a directory. Use offset/limit for large files.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File or directory path (absolute, or relative to the working directory)."},
                    "offset": {"type": "integer", "minimum": 1, "description": "1-based line to start at."},
                    "limit": {"type": "integer", "minimum": 1, "description": "Max lines (default 2000)."}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = parse("read", args)?;
        let path = resolve(&ctx, &a.path);
        let meta = match tokio::fs::metadata(&path).await {
            Ok(m) => m,
            Err(e) => return fail(format!("cannot read {}: {e}", path.display())),
        };
        if meta.is_dir() {
            let mut rd = match tokio::fs::read_dir(&path).await {
                Ok(r) => r,
                Err(e) => return fail(format!("cannot list {}: {e}", path.display())),
            };
            let mut names = Vec::new();
            while let Ok(Some(e)) = rd.next_entry().await {
                let dir = e.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                names.push(format!(
                    "{}{}",
                    e.file_name().to_string_lossy(),
                    if dir { "/" } else { "" }
                ));
            }
            names.sort();
            return Ok(ToolOutput::ok(names.join("\n")));
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return fail(format!("cannot read {}: {e}", path.display())),
        };
        if looks_binary(&bytes) {
            return fail(format!(
                "{} looks binary ({} bytes)",
                path.display(),
                bytes.len()
            ));
        }
        let text = String::from_utf8_lossy(&bytes);
        let start = a.offset.unwrap_or(1).max(1);
        let limit = a.limit.unwrap_or(DEFAULT_LIMIT);
        let mut out = String::new();
        let mut total = 0usize;
        for (i, line) in text.lines().enumerate() {
            total = i + 1;
            if total < start || total >= start + limit {
                continue;
            }
            out.push_str(&format!("{total:>6}\t"));
            if line.len() > MAX_LINE {
                let mut cut = MAX_LINE;
                while !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                out.push_str(&line[..cut]);
                out.push_str("... [line truncated]");
            } else {
                out.push_str(line);
            }
            out.push('\n');
        }
        if total < start && total > 0 {
            return fail(format!("offset {start} past end of file ({total} lines)"));
        }
        if total >= start + limit {
            out.push_str(&format!(
                "[showing lines {start}-{}; file has {total} lines]\n",
                start + limit - 1
            ));
        }
        Ok(ToolOutput::ok(out))
    }
}
