//! Transport-agnostic JSON-RPC peer: request/response correlation plus dispatch of agent-sent
//! requests and notifications to a [`Handler`]. Works over any `AsyncRead`/`AsyncWrite` pair
//! (child stdio in production, `tokio::io::duplex` in tests).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::rpc::{self, Message, RpcError};

/// Agent -> client traffic.
#[async_trait]
pub trait Handler: Send + Sync + 'static {
    /// Called inline on the reader task, in wire order. Must not block.
    fn on_notification(&self, method: &str, params: Value);
    /// Called on its own task per request, so a slow answer (user approval) blocks nothing.
    async fn on_request(&self, method: &str, params: Value) -> Result<Value, RpcError>;
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;

#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("connection closed")]
    Closed,
}

/// Client end of one ACP connection.
#[derive(Debug)]
pub struct Connection {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: AtomicU64,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Connection {
    /// Start reader and writer tasks. Must run inside a tokio runtime.
    pub fn new<R, W>(reader: R, writer: W, handler: Arc<dyn Handler>) -> Self
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let (out, rx) = mpsc::unbounded_channel::<String>();
        let pending: Pending = Arc::default();
        let w = tokio::spawn(write_loop(writer, rx));
        let r = tokio::spawn(read_loop(reader, handler, out.clone(), pending.clone()));
        Self {
            out,
            pending,
            next_id: AtomicU64::new(1),
            tasks: vec![w, r],
        }
    }

    pub async fn call(&self, method: &str, params: &Value) -> Result<Value, CallError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        if self.out.send(rpc::encode_request(id, method, params)).is_err() {
            lock(&self.pending).remove(&id);
            return Err(CallError::Closed);
        }
        match rx.await {
            Ok(r) => r.map_err(CallError::Rpc),
            Err(_) => Err(CallError::Closed),
        }
    }

    /// Fire-and-forget. `false` when the connection is gone.
    pub fn notify(&self, method: &str, params: &Value) -> bool {
        self.out.send(rpc::encode_notification(method, params)).is_ok()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

fn lock(p: &Pending) -> std::sync::MutexGuard<'_, HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>> {
    p.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn write_loop<W: AsyncWrite + Unpin>(mut w: W, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(mut line) = rx.recv().await {
        line.push('\n');
        if w.write_all(line.as_bytes()).await.is_err() || w.flush().await.is_err() {
            break;
        }
    }
    let _ = w.shutdown().await;
}

async fn read_loop<R: AsyncRead + Unpin>(
    r: R,
    handler: Arc<dyn Handler>,
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
) {
    let mut lines = BufReader::new(r).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                tracing::debug!(error = %e, "acp read failed");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match rpc::decode(&line) {
            Ok(Message::Response { id, result }) => {
                let tx = id.as_u64().and_then(|i| lock(&pending).remove(&i));
                match tx {
                    Some(tx) => {
                        let _ = tx.send(result);
                    }
                    None => tracing::debug!(%id, "acp response without pending call"),
                }
            }
            Ok(Message::Notification { method, params }) => handler.on_notification(&method, params),
            Ok(Message::Request { id, method, params }) => {
                let (h, out) = (handler.clone(), out.clone());
                tokio::spawn(async move {
                    let res = h.on_request(&method, params).await;
                    let _ = out.send(rpc::encode_response(&id, &res));
                });
            }
            // Non-JSON noise on stdout (some agents print banners): log, keep going.
            Err(e) => tracing::debug!(error = %e, "acp: undecodable line"),
        }
    }
    // Wake every waiter: dropping the senders yields `Closed`.
    lock(&pending).clear();
}
