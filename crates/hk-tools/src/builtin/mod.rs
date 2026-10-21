//! Builtin tools: read, write, edit, glob, grep, bash, web_fetch, todo.
// port: dsh packages/core/tools builtin set (semantics); opencode tool arg shapes for edit/todo.

mod edit;
mod glob;
mod grep;
mod read;
mod todo;
mod write;

#[cfg(all(feature = "pty", not(target_os = "android")))]
mod bash;
#[cfg(feature = "web-fetch")]
mod web_fetch;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hk_core::Effect;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::registry::ToolRegistry;
use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput};

pub use edit::{EditError, EditTool, apply_edit};
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use todo::{TodoItem, TodoStatus, TodoTool};
pub use write::WriteTool;

#[cfg(all(feature = "pty", not(target_os = "android")))]
pub use bash::BashTool;
#[cfg(feature = "web-fetch")]
pub use web_fetch::WebFetchTool;

/// Every builtin enabled for this build. Dropping the returned effects unregisters them.
pub fn builtin_tools() -> Vec<Arc<dyn Tool>> {
    #[allow(unused_mut)]
    let mut v: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ReadTool),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(GlobTool),
        Arc::new(GrepTool),
        Arc::new(TodoTool::default()),
    ];
    #[cfg(all(feature = "pty", not(target_os = "android")))]
    v.push(Arc::new(BashTool));
    #[cfg(feature = "web-fetch")]
    v.push(Arc::new(WebFetchTool::default()));
    v
}

pub fn register_builtins(reg: &ToolRegistry) -> Result<Vec<Effect>, ToolError> {
    builtin_tools()
        .into_iter()
        .map(|t| reg.register(t))
        .collect()
}

/// Relative paths resolve against the call cwd. Confinement is the sandbox's job (hk-sandbox).
pub(crate) fn resolve(ctx: &ToolCtx, p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        ctx.cwd.clone().unwrap_or_default().join(p)
    }
}

pub(crate) fn base_dir(ctx: &ToolCtx, p: Option<&str>) -> PathBuf {
    match p {
        Some(p) => resolve(ctx, p),
        None => ctx.cwd.clone().unwrap_or_else(|| PathBuf::from(".")),
    }
}

pub(crate) fn parse<T: DeserializeOwned>(tool: &str, args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs {
        tool: tool.to_owned(),
        reason: e.to_string(),
    })
}

/// Recoverable failure the model should see and react to (vs `ToolError`, which is pipeline-level).
pub(crate) fn fail(msg: impl Into<String>) -> Result<ToolOutput, ToolError> {
    Ok(ToolOutput::error(msg))
}

pub(crate) fn join_err(tool: &str, e: tokio::task::JoinError) -> ToolError {
    ToolError::Failed {
        tool: tool.to_owned(),
        reason: e.to_string(),
    }
}

pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8192)].contains(&0)
}
