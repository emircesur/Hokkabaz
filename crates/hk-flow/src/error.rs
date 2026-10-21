use thiserror::Error;

use crate::triggers::wait::WaitRequest;
use crate::version::NodeKey;

#[derive(Debug, Error)]
pub enum FlowError {
    #[error("invalid workflow json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid workflow: {0}")]
    Invalid(String),
    /// Unsupported nodes import as placeholders; only *running* them fails (DESIGN §3.9).
    #[error("node type not supported: {0}")]
    UnsupportedNode(NodeKey),
    #[error("node `{node}` failed: {message}")]
    Node { node: String, message: String },
    /// Not a failure: a node (Wait) asks the executor to park the execution. Only returned
    /// when `NodeHooks::durable_wait` is set; never retried, never recorded as an error.
    #[error("execution suspended (waiting)")]
    Suspend(Box<WaitRequest>),
    #[error("run store: {0}")]
    Store(String),
}
