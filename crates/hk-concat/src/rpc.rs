//! Minimal newline-delimited JSON-RPC 2.0 client for Concat's MCP stdio server.
//!
//! Written instead of reusing `hk-mcp`/rmcp because (a) Concat's server is tiny and fully
//! specified in its `protocol.rs`, (b) hk-mcp's `call_tool` flattens replies to text and drops
//! images/structured content, (c) generic over any `AsyncRead/AsyncWrite` so the whole bridge is
//! unit-testable with `tokio::io::duplex` and no binary.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio::task::JoinHandle;

use crate::ConcatError;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, ConcatError>>>>>;
/// Last stderr lines of the child, for "closed" errors. Bounded; shared with the stderr pump.
pub(crate) type StderrTail = Arc<Mutex<String>>;

const STDERR_TAIL_BYTES: usize = 2000;

pub(crate) struct Rpc {
    writer: AsyncMutex<Box<dyn AsyncWrite + Send + Unpin>>,
    pending: Pending,
    next_id: AtomicU64,
    timeout: Duration,
    stderr: StderrTail,
    reader: JoinHandle<()>,
}

impl Drop for Rpc {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

pub(crate) fn push_tail(tail: &StderrTail, line: &str) {
    let mut t = tail.lock().unwrap_or_else(PoisonError::into_inner);
    t.push_str(line);
    t.push('\n');
    if t.len() > STDERR_TAIL_BYTES {
        let mut cut = t.len() - STDERR_TAIL_BYTES;
        while !t.is_char_boundary(cut) {
            cut += 1;
        }
        t.drain(..cut);
    }
}

impl Rpc {
    pub(crate) fn new<R, W>(reader: R, writer: W, timeout: Duration, stderr: StderrTail) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let pending: Pending = Arc::default();
        let task = {
            let pending = pending.clone();
            let stderr = stderr.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(reader).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    dispatch(&pending, &line);
                }
                let tail = stderr
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                let waiting: Vec<_> = pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .drain()
                    .collect();
                for (_, tx) in waiting {
                    let _ = tx.send(Err(ConcatError::Closed {
                        stderr: tail.clone(),
                    }));
                }
            })
        };
        Self {
            writer: AsyncMutex::new(Box::new(writer)),
            pending,
            next_id: AtomicU64::new(1),
            timeout,
            stderr,
            reader: task,
        }
    }

    pub(crate) async fn request(&self, method: &str, params: Value) -> Result<Value, ConcatError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.write(&msg).await {
            self.pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(ConcatError::Closed {
                stderr: self.tail(),
            }),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
                // Concat's server honours this and stops waiting (protocol.rs `notification`).
                let _ = self
                    .notify(
                        "notifications/cancelled",
                        json!({ "requestId": id, "reason": "timeout" }),
                    )
                    .await;
                Err(ConcatError::Timeout(self.timeout))
            }
        }
    }

    pub(crate) async fn notify(&self, method: &str, params: Value) -> Result<(), ConcatError> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    async fn write(&self, msg: &Value) -> Result<(), ConcatError> {
        let mut line = msg.to_string();
        line.push('\n');
        let mut w = self.writer.lock().await;
        // A dead child shows up as a broken pipe here: report it as Closed with its stderr.
        let res = async {
            w.write_all(line.as_bytes()).await?;
            w.flush().await
        }
        .await;
        res.map_err(|_| ConcatError::Closed {
            stderr: self.tail(),
        })
    }

    /// Closes our write half (child sees EOF on stdin and exits).
    pub(crate) async fn close(&self) {
        let _ = self.writer.lock().await.shutdown().await;
    }

    fn tail(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn dispatch(pending: &Pending, line: &str) {
    let Ok(Value::Object(mut msg)) = serde_json::from_str::<Value>(line) else {
        return;
    };
    // Notifications (progress, logging) and server requests carry a method: ignored. Concat sends
    // none we need; export progress is polled via `export_status`.
    if msg.contains_key("method") {
        return;
    }
    let Some(id) = msg.get("id").and_then(Value::as_u64) else {
        return;
    };
    let Some(tx) = pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&id)
    else {
        return;
    };
    let out = match msg.remove("error") {
        Some(err) => Err(ConcatError::Protocol(format!(
            "{} (code {})",
            err.get("message")
                .and_then(Value::as_str)
                .unwrap_or("error"),
            err.get("code").and_then(Value::as_i64).unwrap_or(0)
        ))),
        None => Ok(msg.remove("result").unwrap_or(Value::Null)),
    };
    let _ = tx.send(out);
}
