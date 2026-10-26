//! Long-term memory (P9.9): small facts the agent saves and recalls across sessions.
//! Stored as JSONL under the data dir; recall is BM25 over the saved text.

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::textindex::bm25;
use super::{fail, parse};
use crate::tool::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: u64,
    /// `user` (everywhere) or `project:<cwd>`.
    pub scope: String,
    pub text: String,
    pub created_ms: u64,
}

/// File-backed memory store (one JSONL file).
#[derive(Debug)]
pub struct MemoryStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl MemoryStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            path: dir.into().join("memory.jsonl"),
            lock: Mutex::new(()),
        }
    }

    pub fn all(&self) -> Vec<MemoryEntry> {
        std::fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn write_all(&self, v: &[MemoryEntry]) -> std::io::Result<()> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let body: String = v
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .map(|l| l + "\n")
            .collect();
        let tmp = self.path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(tmp, &self.path)
    }

    pub fn save(&self, scope: &str, text: &str) -> std::io::Result<MemoryEntry> {
        let _g = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut all = self.all();
        let e = MemoryEntry {
            id: all.iter().map(|e| e.id).max().unwrap_or(0) + 1,
            scope: scope.to_string(),
            text: text.trim().to_string(),
            created_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
        };
        all.push(e.clone());
        self.write_all(&all)?;
        Ok(e)
    }

    pub fn delete(&self, id: u64) -> std::io::Result<bool> {
        let _g = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut all = self.all();
        let n = all.len();
        all.retain(|e| e.id != id);
        if all.len() == n {
            return Ok(false);
        }
        self.write_all(&all)?;
        Ok(true)
    }

    /// Entries visible from `project` (user scope + that project), best match first.
    pub fn search(&self, query: &str, project: Option<&str>, k: usize) -> Vec<MemoryEntry> {
        let visible: Vec<MemoryEntry> = self
            .all()
            .into_iter()
            .filter(|e| {
                e.scope == "user" || project.is_some_and(|p| e.scope == format!("project:{p}"))
            })
            .collect();
        let docs: Vec<&str> = visible.iter().map(|e| e.text.as_str()).collect();
        bm25(query, &docs, k)
            .into_iter()
            .map(|(i, _)| visible[i].clone())
            .collect()
    }
}

/// `memory` tool: save / search / list / delete.
#[derive(Debug)]
pub struct MemoryTool(pub std::sync::Arc<MemoryStore>);

#[derive(Deserialize)]
struct Args {
    action: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    k: Option<usize>,
}

#[async_trait]
impl Tool for MemoryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "memory".into(),
            description: "Long-term memory across sessions. `save` a durable fact or preference \
                (one short sentence; never secrets), `search` before answering questions about the \
                user or project, `list`, `delete` by id."
                .into(),
            parameters: json!({"type":"object","properties":{
                "action":{"type":"string","enum":["save","search","list","delete"]},
                "text":{"type":"string"},"query":{"type":"string"},"id":{"type":"integer"},
                "scope":{"type":"string","enum":["user","project"],"description":"Default project."},
                "k":{"type":"integer","minimum":1,"maximum":20}},
                "required":["action"],"additionalProperties":false}),
        }
    }

    fn risk(&self, _: &Value) -> Risk {
        Risk::Safe
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = parse("memory", args)?;
        let project = ctx.cwd.as_ref().map(|p| p.display().to_string());
        let store = self.0.clone();
        let res = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            match a.action.as_str() {
                "save" => {
                    let text = a
                        .text
                        .filter(|t| !t.trim().is_empty())
                        .ok_or("save needs `text`")?;
                    let scope = match (a.scope.as_deref(), &project) {
                        (Some("user"), _) | (_, None) => "user".to_string(),
                        (_, Some(p)) => format!("project:{p}"),
                    };
                    store
                        .save(&scope, &text)
                        .map(|e| json!(e))
                        .map_err(|e| e.to_string())
                }
                "search" => {
                    let q = a.query.or(a.text).ok_or("search needs `query`")?;
                    Ok(json!(store.search(
                        &q,
                        project.as_deref(),
                        a.k.unwrap_or(5)
                    )))
                }
                "list" => Ok(json!(store.all())),
                "delete" => {
                    let id = a.id.ok_or("delete needs `id`")?;
                    store
                        .delete(id)
                        .map(|ok| json!({"deleted": ok}))
                        .map_err(|e| e.to_string())
                }
                other => Err(format!("unknown action `{other}`")),
            }
        })
        .await
        .map_err(|e| super::join_err("memory", e))?;
        match res {
            Ok(v) => Ok(ToolOutput::ok(v.to_string())),
            Err(e) => fail(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_search_scope_delete() {
        let d = std::env::temp_dir().join(format!("hk_mem_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let s = MemoryStore::new(&d);
        s.save("user", "prefers DeepSeek for coding").unwrap();
        let p = s.save("project:/a", "uses tokio runtime").unwrap();
        s.save("project:/b", "tokio is banned here").unwrap();
        let r = s.search("tokio", Some("/a"), 5);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, p.id);
        assert!(s.delete(p.id).unwrap());
        assert!(s.search("tokio", Some("/a"), 5).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
