//! `AcpEngine`: one ACP agent session, driven turn by turn. Agent updates surface as
//! [`EngineEvent`]s; permission requests go to an [`Approver`]; `fs/*` is confined to the
//! workspace root.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};

use crate::client::{CallError, Connection, Handler};
use crate::proto::{
    ClientCapabilities, ContentBlock, FsCapabilities, Implementation, InitializeParams, InitializeResult,
    NewSessionParams, NewSessionResult, PROTOCOL_VERSION, PermissionOption, PermissionOptionKind,
    PermissionRequest, PlanEntry, PromptParams, PromptResult, ReadTextFileParams, SessionNotification,
    SessionUpdate, StopReason, ToolCallFields, ToolStatus, WriteTextFileParams, tool_content_text,
};
use crate::rpc::RpcError;
use crate::workspace::Workspace;

#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("spawn `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("workspace: {0}")]
    Workspace(#[source] std::io::Error),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("agent connection closed")]
    Closed,
    #[error("protocol: {0}")]
    Protocol(String),
}

impl From<CallError> for AcpError {
    fn from(e: CallError) -> Self {
        match e {
            CallError::Rpc(r) => Self::Rpc(r),
            CallError::Closed => Self::Closed,
        }
    }
}

/// Everything the UI/session layer sees from the agent, in wire order.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    Text(String),
    Thought(String),
    ToolCall(ToolInfo),
    ToolUpdate(ToolInfo),
    Plan(Vec<PlanEntry>),
    /// Agent asked permission; the decision is being collected.
    ApprovalRequested(ApprovalPrompt),
    ApprovalResolved {
        approval_id: String,
        approved: bool,
        /// Why not approved (cancelled, no matching option).
        reason: Option<String>,
    },
}

/// Tool call state as last reported. Updates carry only the changed fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    pub id: String,
    pub title: Option<String>,
    pub kind: Option<String>,
    pub status: Option<ToolStatus>,
    /// Flattened content text, if the update carried content.
    pub content: Option<String>,
    pub raw_input: Option<Value>,
    pub raw_output: Option<Value>,
}

impl From<ToolCallFields> for ToolInfo {
    fn from(f: ToolCallFields) -> Self {
        Self {
            id: f.tool_call_id,
            title: f.title,
            kind: f.kind,
            status: f.status,
            content: f.content.as_deref().map(tool_content_text),
            raw_input: f.raw_input,
            raw_output: f.raw_output,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPrompt {
    pub approval_id: String,
    pub call_id: String,
    pub title: String,
    pub kind: Option<String>,
    /// Compact view of the call input (command, path), bounded.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    AllowOnce,
    AllowAlways,
    Deny,
}

/// Bridge to our approval flow (stdin prompt in the CLI, ApprovalDecision API in the server).
#[async_trait]
pub trait Approver: Send + Sync + 'static {
    async fn decide(&self, prompt: &ApprovalPrompt) -> Decision;
}

/// Denies everything: the safe default when nobody can answer.
#[derive(Debug, Clone, Copy)]
pub struct DenyAll;

#[async_trait]
impl Approver for DenyAll {
    async fn decide(&self, _: &ApprovalPrompt) -> Decision {
        Decision::Deny
    }
}

#[derive(Clone)]
pub struct EngineOptions {
    /// Workspace root: session cwd and the `fs/*` confinement boundary.
    pub cwd: PathBuf,
    pub approver: Arc<dyn Approver>,
    pub client_name: String,
}

impl std::fmt::Debug for EngineOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineOptions").field("cwd", &self.cwd).finish_non_exhaustive()
    }
}

impl EngineOptions {
    pub fn new(cwd: PathBuf, approver: Arc<dyn Approver>) -> Self {
        Self {
            cwd,
            approver,
            client_name: "hokkabaz".to_owned(),
        }
    }
}

#[derive(Debug)]
pub struct AcpEngine {
    conn: Connection,
    session_id: String,
    events: Option<mpsc::UnboundedReceiver<EngineEvent>>,
    cancel_tx: watch::Sender<u64>,
    agent_info: Option<Implementation>,
    #[cfg(not(target_os = "android"))]
    child: Option<tokio::process::Child>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

impl AcpEngine {
    /// Handshake over an existing byte stream: `initialize`, then `session/new`.
    pub async fn connect<R, W>(reader: R, writer: W, opts: EngineOptions) -> Result<Self, AcpError>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let workspace = Workspace::new(&opts.cwd).map_err(AcpError::Workspace)?;
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (cancel_tx, cancel_rx) = watch::channel(0u64);
        let handler = Arc::new(Dispatcher {
            events: ev_tx,
            approver: opts.approver.clone(),
            workspace,
            cancel: cancel_rx,
            next_approval: AtomicU64::new(0),
        });
        let conn = Connection::new(reader, writer, handler);

        let init = InitializeParams {
            protocol_version: PROTOCOL_VERSION,
            client_capabilities: ClientCapabilities {
                fs: FsCapabilities {
                    read_text_file: true,
                    write_text_file: true,
                },
                terminal: false,
            },
            client_info: Implementation {
                name: opts.client_name.clone(),
                title: None,
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
        };
        let res = conn.call("initialize", &to_value(&init)?).await?;
        let init: InitializeResult = from_value(res, "initialize result")?;
        if init.protocol_version != PROTOCOL_VERSION {
            return Err(AcpError::Protocol(format!(
                "agent speaks ACP v{}, client supports v{PROTOCOL_VERSION}",
                init.protocol_version
            )));
        }

        let cwd = opts.cwd.canonicalize().map_err(AcpError::Workspace)?;
        let new = NewSessionParams {
            cwd: cwd.display().to_string(),
            mcp_servers: Vec::new(),
        };
        let res = conn.call("session/new", &to_value(&new)?).await?;
        let new: NewSessionResult = from_value(res, "session/new result")?;

        Ok(Self {
            conn,
            session_id: new.session_id,
            events: Some(ev_rx),
            cancel_tx,
            agent_info: init.agent_info,
            #[cfg(not(target_os = "android"))]
            child: None,
            stderr_tail: Arc::default(),
        })
    }

    /// Spawn the engine process (stdio transport) and handshake. Desktop only.
    #[cfg(not(target_os = "android"))]
    pub async fn spawn(spec: &crate::spec::EngineSpec, opts: EngineOptions) -> Result<Self, AcpError> {
        use std::process::Stdio;
        use tokio::io::{AsyncBufReadExt, BufReader};

        let mut cmd = tokio::process::Command::new(&spec.command);
        cmd.args(&spec.args)
            .envs(&spec.env)
            .current_dir(&opts.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|source| AcpError::Spawn {
            command: spec.command.clone(),
            source,
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(AcpError::Protocol("child stdio not piped".into()));
        };
        // Stderr is diagnostics only: keep a short tail for error messages, never the terminal.
        let tail: Arc<Mutex<VecDeque<String>>> = Arc::default();
        let t = tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                tracing::debug!(target: "hk_acp::agent_stderr", "{l}");
                let mut q = t.lock().unwrap_or_else(PoisonError::into_inner);
                if q.len() >= STDERR_TAIL {
                    q.pop_front();
                }
                q.push_back(l);
            }
        });

        match Self::connect(stdout, stdin, opts).await {
            Ok(mut e) => {
                e.child = Some(child);
                e.stderr_tail = tail;
                Ok(e)
            }
            Err(err) => {
                // Let the stderr task drain what the dying child printed.
                tokio::time::sleep(Duration::from_millis(100)).await;
                let q = tail.lock().unwrap_or_else(PoisonError::into_inner);
                if q.is_empty() {
                    return Err(err);
                }
                let joined = q.iter().cloned().collect::<Vec<_>>().join(" | ");
                Err(AcpError::Protocol(format!("{err}; agent stderr: {joined}")))
            }
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn agent_info(&self) -> Option<&Implementation> {
        self.agent_info.as_ref()
    }

    /// Last lines the child wrote to stderr (spawned engines only).
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    /// The event stream; yields `Some` once. Drain it while `prompt` runs.
    pub fn take_events(&mut self) -> Option<mpsc::UnboundedReceiver<EngineEvent>> {
        self.events.take()
    }

    /// Send a user prompt and wait for the turn to end.
    pub async fn prompt(&self, text: &str) -> Result<StopReason, AcpError> {
        let p = PromptParams {
            session_id: self.session_id.clone(),
            prompt: vec![ContentBlock::text(text)],
        };
        let res = self.conn.call("session/prompt", &to_value(&p)?).await?;
        Ok(from_value::<PromptResult>(res, "session/prompt result")?.stop_reason)
    }

    /// Ask the agent to stop the turn. Parked permission requests answer `cancelled`, as the
    /// spec requires; the pending `prompt` then resolves with `StopReason::Cancelled`.
    pub fn cancel(&self) {
        self.cancel_tx.send_modify(|g| *g += 1);
        self.conn.notify("session/cancel", &json!({"sessionId": self.session_id}));
    }

    /// Close stdin (agent sees EOF), give the child a moment, then kill it.
    pub async fn shutdown(self) {
        let Self {
            conn,
            #[cfg(not(target_os = "android"))]
            child,
            ..
        } = self;
        drop(conn);
        #[cfg(not(target_os = "android"))]
        if let Some(mut c) = child
            && tokio::time::timeout(Duration::from_secs(2), c.wait()).await.is_err()
        {
            let _ = c.kill().await;
        }
    }
}

#[cfg(not(target_os = "android"))]
const STDERR_TAIL: usize = 20;

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, AcpError> {
    serde_json::to_value(v).map_err(|e| AcpError::Protocol(e.to_string()))
}

fn from_value<T: serde::de::DeserializeOwned>(v: Value, what: &str) -> Result<T, AcpError> {
    serde_json::from_value(v).map_err(|e| AcpError::Protocol(format!("{what}: {e}")))
}

struct Dispatcher {
    events: mpsc::UnboundedSender<EngineEvent>,
    approver: Arc<dyn Approver>,
    workspace: Workspace,
    cancel: watch::Receiver<u64>,
    next_approval: AtomicU64,
}

#[async_trait]
impl Handler for Dispatcher {
    fn on_notification(&self, method: &str, params: Value) {
        if method != "session/update" {
            tracing::debug!(method, "acp: ignoring notification");
            return;
        }
        let n: SessionNotification = match serde_json::from_value(params) {
            Ok(n) => n,
            Err(e) => {
                tracing::debug!(error = %e, "acp: bad session/update");
                return;
            }
        };
        let ev = match n.update {
            SessionUpdate::AgentMessageChunk { content } => content.as_text().map(|t| EngineEvent::Text(t.to_owned())),
            SessionUpdate::AgentThoughtChunk { content } => {
                content.as_text().map(|t| EngineEvent::Thought(t.to_owned()))
            }
            SessionUpdate::ToolCall(f) => Some(EngineEvent::ToolCall(f.into())),
            SessionUpdate::ToolCallUpdate(f) => Some(EngineEvent::ToolUpdate(f.into())),
            SessionUpdate::Plan { entries } => Some(EngineEvent::Plan(entries)),
            // Our own prompt echoed back, command/mode updates: nothing to show.
            SessionUpdate::UserMessageChunk { .. } | SessionUpdate::Other => None,
        };
        if let Some(ev) = ev {
            let _ = self.events.send(ev);
        }
    }

    async fn on_request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "session/request_permission" => {
                let req = serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
                Ok(self.permission(req).await)
            }
            "fs/read_text_file" => {
                let p: ReadTextFileParams =
                    serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
                let content = self.workspace.read_text(&p.path, p.line, p.limit)?;
                Ok(json!({"content": content}))
            }
            "fs/write_text_file" => {
                let p: WriteTextFileParams =
                    serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))?;
                self.workspace.write_text(&p.path, &p.content)?;
                Ok(Value::Null)
            }
            // terminal/* and anything newer: declined capability, polite refusal.
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

impl Dispatcher {
    async fn permission(&self, req: PermissionRequest) -> Value {
        let approval_id = format!("appr_{}", self.next_approval.fetch_add(1, Ordering::Relaxed));
        let info: ToolInfo = req.tool_call.into();
        let prompt = ApprovalPrompt {
            approval_id: approval_id.clone(),
            call_id: info.id.clone(),
            title: info.title.clone().unwrap_or_else(|| info.kind.clone().unwrap_or_else(|| "tool".into())),
            kind: info.kind.clone(),
            detail: detail_of(&info),
        };
        let _ = self.events.send(EngineEvent::ApprovalRequested(prompt.clone()));

        let mut cancel = self.cancel.clone();
        cancel.borrow_and_update();
        let decision = tokio::select! {
            d = self.approver.decide(&prompt) => Some(d),
            _ = cancel.changed() => None,
        };

        let (outcome, approved, reason) = match decision {
            None => (cancelled(), false, Some("cancelled".to_owned())),
            Some(d) => match pick_option(&req.options, d) {
                Some(opt) => (
                    json!({"outcome": "selected", "optionId": opt.option_id}),
                    opt.kind.is_allow(),
                    None,
                ),
                None => (
                    cancelled(),
                    false,
                    Some(match d {
                        Decision::Deny => "denied (agent offered no reject option)".to_owned(),
                        _ => "agent offered no matching allow option".to_owned(),
                    }),
                ),
            },
        };
        let _ = self.events.send(EngineEvent::ApprovalResolved {
            approval_id,
            approved,
            reason,
        });
        json!({"outcome": outcome})
    }
}

fn cancelled() -> Value {
    json!({"outcome": "cancelled"})
}

/// Map our decision onto the options the agent offered.
pub fn pick_option(options: &[PermissionOption], d: Decision) -> Option<&PermissionOption> {
    use PermissionOptionKind::{AllowAlways, AllowOnce, RejectAlways, RejectOnce};
    let find = |k: PermissionOptionKind| options.iter().find(|o| o.kind == k);
    match d {
        Decision::AllowOnce => find(AllowOnce).or_else(|| find(AllowAlways)),
        Decision::AllowAlways => find(AllowAlways).or_else(|| find(AllowOnce)),
        Decision::Deny => find(RejectOnce).or_else(|| find(RejectAlways)),
    }
}

fn detail_of(t: &ToolInfo) -> Option<String> {
    let s = match (&t.raw_input, &t.content) {
        (Some(v), _) => v.to_string(),
        (None, Some(c)) if !c.is_empty() => c.clone(),
        _ => return None,
    };
    Some(if s.chars().count() > 300 {
        let mut c: String = s.chars().take(300).collect();
        c.push_str("...");
        c
    } else {
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(id: &str, kind: PermissionOptionKind) -> PermissionOption {
        PermissionOption {
            option_id: id.into(),
            name: id.into(),
            kind,
        }
    }

    #[test]
    fn option_picking() {
        use PermissionOptionKind::*;
        let all = [opt("a", AllowOnce), opt("A", AllowAlways), opt("r", RejectOnce)];
        assert_eq!(pick_option(&all, Decision::AllowOnce).unwrap().option_id, "a");
        assert_eq!(pick_option(&all, Decision::AllowAlways).unwrap().option_id, "A");
        assert_eq!(pick_option(&all, Decision::Deny).unwrap().option_id, "r");
        let only_allow = [opt("A", AllowAlways)];
        assert_eq!(pick_option(&only_allow, Decision::AllowOnce).unwrap().option_id, "A");
        assert!(pick_option(&only_allow, Decision::Deny).is_none());
    }
}
