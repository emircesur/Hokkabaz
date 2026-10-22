//! [`ConcatBackend`] over a `concat --mcp [auto|headless|window]` child process.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex as AsyncMutex;

use crate::backend::{BackendType, ConcatBackend, ReplyImage, ToolReply};
use crate::rpc::Rpc;
use crate::{ConcatConfig, ConcatError};

/// Protocol eras Concat's stdio server answers `initialize` for (concat-mcp `protocol.rs`).
const LEGACY_PROTOCOLS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const CLIENT_PROTOCOL: &str = "2025-06-18";

pub struct ProcessBackend {
    rpc: Rpc,
    child: AsyncMutex<Option<tokio::process::Child>>,
    server_version: String,
}

impl fmt::Debug for ProcessBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessBackend")
            .field("server_version", &self.server_version)
            .finish_non_exhaustive()
    }
}

impl ProcessBackend {
    /// Locates and starts Concat (desktop only; android is `Unsupported`).
    #[cfg(not(target_os = "android"))]
    pub async fn spawn(cfg: &ConcatConfig) -> Result<Self, ConcatError> {
        use std::process::Stdio;

        let bin = crate::locate_binary(cfg.path.as_deref())?;
        let mut cmd = tokio::process::Command::new(&bin);
        cmd.arg("--mcp").arg(cfg.mode.as_arg());
        cmd.env_clear();
        // Allowlist like hk-mcp, plus the dirs Concat uses to find its `automation.json` (window
        // mode). Host secrets (API keys etc.) are deliberately not inherited.
        for k in [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "LANG",
            "LC_ALL",
            "TMPDIR",
            "TEMP",
            "TMP",
            "SYSTEMROOT",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
            "CONCAT_MCP_ROOTS",
        ] {
            if let Some(v) = std::env::var_os(k) {
                cmd.env(k, v);
            }
        }
        if !cfg.roots.is_empty() {
            let joined = std::env::join_paths(&cfg.roots)
                .map_err(|e| ConcatError::Spawn(format!("`[concat] roots`: {e}")))?;
            cmd.env("CONCAT_MCP_ROOTS", joined);
        }
        match (&cfg.api_address, &cfg.api_token) {
            (Some(a), Some(t)) => {
                cmd.env("CONCAT_API_ADDRESS", a).env("CONCAT_API_TOKEN", t);
            }
            (None, None) => {}
            _ => {
                return Err(ConcatError::Spawn(
                    "`[concat] api_address` and `api_token` go together".into(),
                ));
            }
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        let mut child = cmd
            .spawn()
            .map_err(|e| ConcatError::Spawn(format!("{}: {e}", bin.display())))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(ConcatError::Spawn("child stdio unavailable".into()));
        };
        let tail: crate::rpc::StderrTail = Arc::default();
        {
            use tokio::io::AsyncBufReadExt;
            let tail = tail.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    tracing::debug!(target: "hk_concat::stderr", "{l}");
                    crate::rpc::push_tail(&tail, &l);
                }
            });
        }
        let mut backend = Self::handshake(Rpc::new(stdout, stdin, cfg.timeout(), tail)).await?;
        *backend.child.get_mut() = Some(child);
        Ok(backend)
    }

    #[cfg(target_os = "android")]
    pub async fn spawn(_cfg: &ConcatConfig) -> Result<Self, ConcatError> {
        Err(ConcatError::Unsupported(
            "the Concat process bridge is desktop-only (no `concat` binary on android)",
        ))
    }

    /// Speaks MCP over caller-provided streams (tests, or a future socket transport).
    pub async fn from_streams<R, W>(
        reader: R,
        writer: W,
        timeout: Duration,
    ) -> Result<Self, ConcatError>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self::handshake(Rpc::new(reader, writer, timeout, Arc::default())).await
    }

    async fn handshake(rpc: Rpc) -> Result<Self, ConcatError> {
        let init = rpc
            .request(
                "initialize",
                json!({
                    "protocolVersion": CLIENT_PROTOCOL,
                    "capabilities": {},
                    "clientInfo": { "name": "hokkabaz", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        let proto = init
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !LEGACY_PROTOCOLS.contains(&proto) {
            return Err(ConcatError::Protocol(format!(
                "server speaks unsupported MCP version `{proto}`"
            )));
        }
        rpc.notify("notifications/initialized", json!({})).await?;
        let server_version = init
            .pointer("/serverInfo/version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        Ok(Self {
            rpc,
            child: AsyncMutex::new(None),
            server_version,
        })
    }

    /// Version string from the server's `initialize` reply.
    pub fn server_version(&self) -> &str {
        &self.server_version
    }
}

#[async_trait]
impl ConcatBackend for ProcessBackend {
    fn kind(&self) -> BackendType {
        BackendType::Process
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<ToolReply, ConcatError> {
        let res = self
            .rpc
            .request("tools/call", json!({ "name": name, "arguments": args }))
            .await?;
        let reply = parse_call_result(&res);
        if res.get("isError").and_then(Value::as_bool).unwrap_or(false) {
            return Err(ConcatError::from_tool_text(name, &reply.text));
        }
        Ok(reply)
    }

    async fn shutdown(&self) {
        self.rpc.close().await;
        if let Some(mut child) = self.child.lock().await.take() {
            // EOF on stdin ends `concat --mcp` after the request in flight; do not wait forever.
            if tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
        }
    }
}

/// `{content:[{type:text|image}], structuredContent?}` -> [`ToolReply`].
pub(crate) fn parse_call_result(res: &Value) -> ToolReply {
    let mut text = String::new();
    let mut images = Vec::new();
    for block in res
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
            }
            Some("image") => images.push(ReplyImage {
                mime: block
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("image/png")
                    .to_owned(),
                data: block
                    .get("data")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            }),
            _ => {}
        }
    }
    // Concat puts the same value in both; arrays (project_list) only arrive as text.
    let value = res
        .get("structuredContent")
        .cloned()
        .or_else(|| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| Value::String(text.clone()));
    ToolReply {
        value,
        text,
        images,
    }
}
