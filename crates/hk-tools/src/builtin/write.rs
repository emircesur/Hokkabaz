use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{fail, parse, resolve};
use crate::tool::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};

#[derive(Debug)]
pub struct WriteTool;

#[derive(Deserialize)]
struct Args {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".into(),
            description: "Create or overwrite a file with the given content. Creates parent directories. Prefer `edit` for changes to existing files.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        }
    }

    fn risk(&self, _: &Value) -> Risk {
        Risk::Ask
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = parse("write", args)?;
        let path = resolve(&ctx, &a.path);
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
            && let Err(e) = tokio::fs::create_dir_all(dir).await
        {
            return fail(format!("cannot create {}: {e}", dir.display()));
        }
        match tokio::fs::write(&path, a.content.as_bytes()).await {
            Ok(()) => Ok(ToolOutput::ok(format!(
                "wrote {} bytes to {}",
                a.content.len(),
                path.display()
            ))),
            Err(e) => fail(format!("cannot write {}: {e}", path.display())),
        }
    }
}
