//! In-process Concat engine (cargo feature `engine`, off by default).
//!
//! UNVERIFIED: written against the source of `third_party/concat/src/crates/concat-mcp` and
//! `concat-api` but never compiled in this repo's CI-less environment (FFmpeg 7 dev libs missing,
//! see docs/CONCAT.md). It reuses Concat's own `Toolbox` + `Headless` API exactly like `concat
//! --mcp headless` does, so tool names/semantics are identical to [`crate::ProcessBackend`] by
//! construction; only the transport (a worker thread instead of a child process) differs.
//!
//! A dedicated OS thread owns the `Toolbox` and the API: Concat's API is synchronous and
//! single-threaded by design (one request at a time), and this avoids any `Send`/`Sync`
//! requirement on its types.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::thread::JoinHandle;

use async_trait::async_trait;
use concat_mcp::Backend as _;
use concat_mcp::tools::{Context, Toolbox};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::ConcatError;
use crate::backend::{BackendType, ConcatBackend, ToolReply};

struct Job {
    name: String,
    args: Value,
    reply: oneshot::Sender<Result<ToolReply, ConcatError>>,
}

#[derive(Debug)]
pub struct EngineBackend {
    tx: Mutex<Option<mpsc::Sender<Job>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl EngineBackend {
    /// Starts the engine writing under `$HOME`.
    pub fn start() -> Result<Self, ConcatError> {
        Self::start_with_roots(Vec::new())
    }

    /// Starts the engine confined to `roots` (empty: the user's home, Concat's default).
    pub fn start_with_roots(roots: Vec<PathBuf>) -> Result<Self, ConcatError> {
        let (tx, rx) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
        let worker = std::thread::Builder::new()
            .name("hk-concat-engine".into())
            .spawn(move || {
                let started = (|| -> Result<concat_mcp::Headless, String> {
                    let mut api = concat_api::Api::new(std::sync::Arc::new(|_| {}))?;
                    if !roots.is_empty() {
                        api.restrict_writes_to(roots);
                    }
                    Ok(concat_mcp::Headless::with_api(api))
                })();
                let mut backend = match started {
                    Ok(b) => {
                        let _ = ready_tx.send(Ok(()));
                        b
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let toolbox = Toolbox::new();
                let cancelled = AtomicBool::new(false);
                for job in rx {
                    let outcome = toolbox.call(
                        &job.name,
                        job.args,
                        Context {
                            backend: &mut backend,
                            cancelled: &cancelled,
                            progress: None,
                        },
                    );
                    let reply = match outcome {
                        None => Err(ConcatError::Tool {
                            tool: job.name.clone(),
                            code: Some("invalid".into()),
                            message: format!("unknown Concat tool `{}`", job.name),
                        }),
                        Some(o) => {
                            let mut res = json!({ "content": o.content, "isError": o.is_error });
                            if let Some(s) = o.structured {
                                res["structuredContent"] = s;
                            }
                            let reply = crate::process::parse_call_result(&res);
                            if o.is_error {
                                Err(ConcatError::from_tool_text(&job.name, &reply.text))
                            } else {
                                Ok(reply)
                            }
                        }
                    };
                    let _ = job.reply.send(reply);
                }
                backend.finish();
            })
            .map_err(|e| ConcatError::Spawn(format!("engine thread: {e}")))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                tx: Mutex::new(Some(tx)),
                worker: Mutex::new(Some(worker)),
            }),
            Ok(Err(e)) => Err(ConcatError::Spawn(format!("Concat engine: {e}"))),
            Err(_) => Err(ConcatError::Spawn(
                "Concat engine thread died during start".into(),
            )),
        }
    }
}

#[async_trait]
impl ConcatBackend for EngineBackend {
    fn kind(&self) -> BackendType {
        BackendType::Engine
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<ToolReply, ConcatError> {
        let (reply, rx) = oneshot::channel();
        {
            let g = self
                .tx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let tx = g.as_ref().ok_or_else(|| ConcatError::Closed {
                stderr: String::new(),
            })?;
            tx.send(Job {
                name: name.to_owned(),
                args,
                reply,
            })
            .map_err(|_| ConcatError::Closed {
                stderr: String::new(),
            })?;
        }
        rx.await.map_err(|_| ConcatError::Closed {
            stderr: "engine worker panicked".into(),
        })?
    }

    async fn shutdown(&self) {
        drop(
            self.tx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(w) = worker {
            let _ = tokio::task::spawn_blocking(move || w.join()).await;
        }
    }
}
