use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{fail, parse, resolve};
use crate::tool::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};

#[derive(Debug)]
pub struct EditTool;

#[derive(Deserialize)]
struct Args {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("old_string is empty")]
    EmptyOld,
    #[error("old_string and new_string are identical")]
    NoChange,
    #[error("old_string not found")]
    NotFound,
    #[error("old_string matches {0} places; add context to make it unique or set replace_all")]
    Ambiguous(usize),
}

/// Exact-substring replace. Returns new content and replacement count.
pub fn apply_edit(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, usize), EditError> {
    if old.is_empty() {
        return Err(EditError::EmptyOld);
    }
    if old == new {
        return Err(EditError::NoChange);
    }
    let n = content.matches(old).count();
    match n {
        0 => Err(EditError::NotFound),
        1 => Ok((content.replacen(old, new, 1), 1)),
        _ if replace_all => Ok((content.replace(old, new), n)),
        _ => Err(EditError::Ambiguous(n)),
    }
}

#[async_trait]
impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Replace an exact string in a file. old_string must match exactly once unless replace_all is set. Read the file first.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string", "description": "Exact text to replace."},
                    "new_string": {"type": "string", "description": "Replacement text."},
                    "replace_all": {"type": "boolean", "description": "Replace every occurrence."}
                },
                "required": ["path", "old_string", "new_string"],
                "additionalProperties": false
            }),
        }
    }

    fn risk(&self, _: &Value) -> Risk {
        Risk::Ask
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = parse("edit", args)?;
        let path = resolve(&ctx, &a.path);
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return fail(format!("cannot read {}: {e}", path.display())),
        };
        let (new, n) = match apply_edit(&content, &a.old_string, &a.new_string, a.replace_all) {
            Ok(r) => r,
            Err(e) => return fail(format!("edit failed: {e}")),
        };
        match tokio::fs::write(&path, new).await {
            Ok(()) => Ok(ToolOutput::ok(format!(
                "replaced {n} occurrence(s) in {}",
                path.display()
            ))),
            Err(e) => fail(format!("cannot write {}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_replace() {
        assert_eq!(
            apply_edit("a b c", "b", "X", false),
            Ok(("a X c".into(), 1))
        );
    }

    #[test]
    fn ambiguous_needs_replace_all() {
        assert_eq!(
            apply_edit("x x", "x", "y", false),
            Err(EditError::Ambiguous(2))
        );
        assert_eq!(apply_edit("x x", "x", "y", true), Ok(("y y".into(), 2)));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(apply_edit("abc", "z", "y", false), Err(EditError::NotFound));
        assert_eq!(apply_edit("abc", "", "y", false), Err(EditError::EmptyOld));
        assert_eq!(apply_edit("abc", "a", "a", false), Err(EditError::NoChange));
    }

    #[tokio::test]
    async fn tool_edits_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "hello world").unwrap();
        let ctx = ToolCtx {
            cwd: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let out = EditTool
            .execute(
                ctx.clone(),
                json!({"path":"f.txt","old_string":"world","new_string":"rust"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello rust");
        let out = EditTool
            .execute(
                ctx,
                json!({"path":"f.txt","old_string":"nope","new_string":"x"}),
            )
            .await
            .unwrap();
        assert!(out.is_error);
    }
}
