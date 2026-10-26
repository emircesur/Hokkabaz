//! bash over a PTY (desktop/daemon only; compiled out on android).
use std::io::Read;
use std::time::Duration;

use async_trait::async_trait;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{base_dir, fail, join_err, parse};
use crate::tool::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;
/// Pipeline truncates what the model sees; this only bounds memory.
const MAX_CAPTURE: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub struct BashTool;

#[derive(Deserialize)]
struct Args {
    command: String,
    timeout_ms: Option<u64>,
}

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: "Run a shell command in a PTY in the working directory. Returns combined output and exit code. Default timeout 120s.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_MS}
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        }
    }

    fn risk(&self, _: &Value) -> Risk {
        Risk::Ask
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = parse("bash", args)?;
        let timeout = Duration::from_millis(
            a.timeout_ms
                .unwrap_or(DEFAULT_TIMEOUT_MS)
                .min(MAX_TIMEOUT_MS),
        );
        let cwd = base_dir(&ctx, None);

        let pair = match native_pty_system().openpty(PtySize {
            rows: 24,
            cols: 200,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(p) => p,
            Err(e) => return fail(format!("cannot open pty: {e}")),
        };
        let mut cmd = if cfg!(windows) {
            let mut c = CommandBuilder::new("cmd");
            c.arg("/C");
            c
        } else {
            let mut c = CommandBuilder::new("sh");
            c.arg("-c");
            c
        };
        cmd.arg(&a.command);
        cmd.cwd(&cwd);
        cmd.env("TERM", "dumb");
        let mut child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c,
            Err(e) => return fail(format!("cannot spawn: {e}")),
        };
        // Slave must close in the parent or the reader never sees EOF.
        drop(pair.slave);
        let mut reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => return fail(format!("cannot read pty: {e}")),
        };
        // Dropping the call (turn cancelled / tool skipped) must not leave the process running.
        let mut killer = KillOnDrop(Some(child.clone_killer()));

        let read = tokio::task::spawn_blocking(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            // EIO after the child exits is the normal EOF on Linux ptys.
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                if buf.len() < MAX_CAPTURE {
                    buf.extend_from_slice(&chunk[..n.min(MAX_CAPTURE - buf.len())]);
                }
            }
            buf
        });
        let wait = tokio::task::spawn_blocking(move || child.wait());

        let (code, timed_out) = match tokio::time::timeout(timeout, wait).await {
            Ok(joined) => match joined.map_err(|e| join_err("bash", e))? {
                Ok(status) => (Some(status.exit_code()), false),
                Err(e) => return fail(format!("wait failed: {e}")),
            },
            Err(_) => {
                killer.kill();
                (None, true)
            }
        };
        killer.0 = None;
        // Grandchildren may keep the pty open; don't hang on them.
        let buf = match tokio::time::timeout(Duration::from_secs(1), read).await {
            Ok(joined) => joined.map_err(|e| join_err("bash", e))?,
            Err(_) => Vec::new(),
        };
        drop(pair.master);

        let mut text = clean(&String::from_utf8_lossy(&buf));
        if timed_out {
            text.push_str(&format!(
                "\n[timed out after {}ms; killed]",
                timeout.as_millis()
            ));
            return Ok(ToolOutput::error(text));
        }
        let code = code.unwrap_or(0);
        text.push_str(&format!("\n[exit code: {code}]"));
        Ok(if code == 0 {
            ToolOutput::ok(text)
        } else {
            ToolOutput::error(text)
        })
    }
}

/// PTY output has CRLF line ends and may carry ANSI escapes; neither helps the model.
fn clean(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' if it.peek() == Some(&'[') => {
                it.next();
                for n in it.by_ref() {
                    if ('\x40'..='\x7e').contains(&n) {
                        break;
                    }
                }
            }
            '\r' => {}
            _ => out.push(c),
        }
    }
    out.trim_end().to_owned()
}

/// Kills the child when dropped while armed.
struct KillOnDrop(Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>);

impl KillOnDrop {
    fn kill(&mut self) {
        if let Some(k) = self.0.as_mut() {
            let _ = k.kill();
        }
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        self.kill();
    }
}
