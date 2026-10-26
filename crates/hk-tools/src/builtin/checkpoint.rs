//! Checkpoints & rewind (P9.19): snapshots of a working tree in a shadow git repo under the
//! data dir, so the user's own `.git` is never touched. Needs `git` on PATH (desktop).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{base_dir, fail, parse};
use crate::tool::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub label: String,
    pub ts: String,
}

#[derive(Debug)]
pub struct Checkpoints {
    dir: PathBuf,
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

impl Checkpoints {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: data_dir.into().join("checkpoints"),
        }
    }

    fn git(&self, tree: &Path, args: &[&str]) -> std::io::Result<String> {
        let gd = self
            .dir
            .join(format!("{:016x}", fnv(&tree.display().to_string())));
        if !gd.exists() {
            std::fs::create_dir_all(&gd)?;
            let o = std::process::Command::new("git")
                .arg("init")
                .arg("--bare")
                .arg("-q")
                .arg(&gd)
                .output()?;
            if !o.status.success() {
                return Err(std::io::Error::other(
                    String::from_utf8_lossy(&o.stderr).into_owned(),
                ));
            }
        }
        let o = std::process::Command::new("git")
            .arg(format!("--git-dir={}", gd.display()))
            .arg(format!("--work-tree={}", tree.display()))
            .args([
                "-c",
                "user.name=hokkabaz",
                "-c",
                "user.email=hokkabaz@localhost",
                "-c",
                "core.autocrlf=false",
            ])
            .args(args)
            .output()?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).into_owned())
        } else {
            Err(std::io::Error::other(
                String::from_utf8_lossy(&o.stderr).into_owned(),
            ))
        }
    }

    /// Snapshot everything not ignored by the tree's `.gitignore`.
    pub fn create(&self, tree: &Path, label: &str) -> std::io::Result<Checkpoint> {
        self.git(tree, &["add", "-A", "--", "."])?;
        self.git(tree, &["commit", "-q", "--allow-empty", "-m", label])?;
        self.list(tree)?
            .into_iter()
            .next()
            .ok_or_else(|| std::io::Error::other("no checkpoint"))
    }

    pub fn list(&self, tree: &Path) -> std::io::Result<Vec<Checkpoint>> {
        let out = match self.git(tree, &["log", "-n", "50", "--format=%H%x09%cI%x09%s"]) {
            Ok(o) => o,
            // Fresh shadow repo without commits.
            Err(_) => return Ok(Vec::new()),
        };
        Ok(out
            .lines()
            .filter_map(|l| {
                let mut p = l.splitn(3, '\t');
                Some(Checkpoint {
                    id: p.next()?.into(),
                    ts: p.next()?.into(),
                    label: p.next().unwrap_or("").into(),
                })
            })
            .collect())
    }

    /// Restore tracked files to `id` and remove files created after it.
    pub fn restore(&self, tree: &Path, id: &str) -> std::io::Result<()> {
        if !id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(std::io::Error::other("bad checkpoint id"));
        }
        // Safety net: the state being replaced is itself recoverable.
        self.create(tree, "before restore")?;
        self.git(tree, &["read-tree", "-u", "--reset", id])?;
        self.git(tree, &["clean", "-fdq", "--", "."])?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct CheckpointTool(pub Arc<Checkpoints>);

#[derive(Deserialize)]
struct Args {
    action: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for CheckpointTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "checkpoint".into(),
            description: "Snapshot the working directory before risky edits (`create`), `list` \
                snapshots, or `restore` one (current state is saved first)."
                .into(),
            parameters: json!({"type":"object","properties":{
                "action":{"type":"string","enum":["create","list","restore"]},
                "label":{"type":"string"},"id":{"type":"string"},"path":{"type":"string"}},
                "required":["action"],"additionalProperties":false}),
        }
    }

    fn risk(&self, args: &Value) -> Risk {
        if args.get("action").and_then(Value::as_str) == Some("restore") {
            Risk::Ask
        } else {
            Risk::Safe
        }
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = parse("checkpoint", args)?;
        let tree = base_dir(&ctx, a.path.as_deref());
        let cp = self.0.clone();
        let r = tokio::task::spawn_blocking(move || -> std::io::Result<Value> {
            Ok(match a.action.as_str() {
                "create" => json!(cp.create(&tree, a.label.as_deref().unwrap_or("checkpoint"))?),
                "list" => json!(cp.list(&tree)?),
                "restore" => {
                    cp.restore(&tree, a.id.as_deref().unwrap_or_default())?;
                    json!({"restored": a.id})
                }
                other => return Err(std::io::Error::other(format!("unknown action `{other}`"))),
            })
        })
        .await
        .map_err(|e| super::join_err("checkpoint", e))?;
        match r {
            Ok(v) => Ok(ToolOutput::ok(v.to_string())),
            Err(e) => fail(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_restore() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let base = std::env::temp_dir().join(format!("hk_cp_{}", std::process::id()));
        let tree = base.join("tree");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("a.txt"), "v1").unwrap();
        let cps = Checkpoints::new(base.join("data"));
        let c1 = cps.create(&tree, "first").unwrap();
        std::fs::write(tree.join("a.txt"), "v2").unwrap();
        std::fs::write(tree.join("new.txt"), "x").unwrap();
        cps.restore(&tree, &c1.id).unwrap();
        assert_eq!(std::fs::read_to_string(tree.join("a.txt")).unwrap(), "v1");
        assert!(!tree.join("new.txt").exists());
        assert!(cps.list(&tree).unwrap().len() >= 2);
        let _ = std::fs::remove_dir_all(&base);
    }
}
