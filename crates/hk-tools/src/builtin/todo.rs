use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::parse;
use crate::tool::{Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Deserialize)]
struct Args {
    todos: Vec<TodoItem>,
}

/// Whole-list replace per call, kept per session. The call itself is in the session log, so
/// the list is reconstructable from there; this copy only serves UI/inspection.
#[derive(Debug, Default)]
pub struct TodoTool {
    lists: Mutex<HashMap<String, Vec<TodoItem>>>,
}

impl TodoTool {
    pub fn items(&self, session_id: &str) -> Vec<TodoItem> {
        self.lists
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "todo".into(),
            description: "Replace the task list for this session. Send the full list every call; keep at most one item in_progress.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string"},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                            },
                            "required": ["content", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["todos"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = parse("todo", args)?;
        let done = a
            .todos
            .iter()
            .filter(|t| t.status == TodoStatus::Completed)
            .count();
        let out = format!("todo list updated ({done}/{} completed)", a.todos.len());
        let key = ctx.session_id.unwrap_or_default();
        self.lists
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, a.todos);
        Ok(ToolOutput::ok(out))
    }
}
