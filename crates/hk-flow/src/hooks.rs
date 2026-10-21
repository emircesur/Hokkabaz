//! Host hooks the executor hands to nodes: things a node needs from the embedder
//! (sub-workflow runner, webhook response sink, credential lookup) and per-execution
//! scratch state. Nodes never reach the store or server directly.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::error::FlowError;
use crate::item::Item;
use crate::workflow::CredentialRef;

// ------------------------------------------------------------- sub-workflows

/// How an Execute Workflow node wants its child run.
#[derive(Debug, Clone)]
pub struct SubWorkflowRequest {
    /// Id of a stored workflow (`source = database`).
    pub workflow_id: Option<String>,
    /// Inline workflow JSON (`source = parameter`).
    pub workflow_json: Option<Value>,
    pub items: Vec<Item>,
}

/// Stub seam for Execute Workflow. The server/daemon supplies the real runner; hk-flow
/// has no workflow store.
#[async_trait]
pub trait SubWorkflowRunner: Send + Sync + fmt::Debug {
    /// Items the child workflow returned (its last node's first output).
    async fn run(&self, req: SubWorkflowRequest) -> Result<Vec<Item>, FlowError>;
}

// ------------------------------------------------------------ webhook response

#[derive(Debug, Clone, PartialEq)]
pub enum ResponseBody {
    Empty,
    Json(Value),
    Text(String),
    Binary { mime_type: String, data: Vec<u8> },
    Redirect(String),
}

/// What a Respond to Webhook node asks the server to answer with.
#[derive(Debug, Clone, PartialEq)]
pub struct WebhookResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: ResponseBody,
}

/// Executor-side receiver for [`WebhookResponse`]; the webhook handler waits on it.
pub trait ResponseSink: Send + Sync + fmt::Debug {
    fn respond(&self, response: WebhookResponse);
}

/// Sink that keeps responses for later inspection (tests, simple embedders).
#[derive(Debug, Default)]
pub struct CollectingSink(Mutex<Vec<WebhookResponse>>);

impl CollectingSink {
    pub fn take(&self) -> Vec<WebhookResponse> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl ResponseSink for CollectingSink {
    fn respond(&self, response: WebhookResponse) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(response);
    }
}

// ----------------------------------------------------------------- credentials

/// Resolves a node's credential reference to its secret fields (`user`, `password`,
/// `token`, `name`/`value`, ...). Secrets are used to build a request and dropped; they
/// must not be copied into items, errors or logs.
#[async_trait]
pub trait CredentialResolver: Send + Sync + fmt::Debug {
    async fn resolve(
        &self,
        credential_type: &str,
        reference: &CredentialRef,
    ) -> Result<Map<String, Value>, FlowError>;
}

// ----------------------------------------------------------------------- bundle

#[derive(Debug, Clone)]
pub struct NodeHooks {
    pub sub_workflow: Option<Arc<dyn SubWorkflowRunner>>,
    pub response: Option<Arc<dyn ResponseSink>>,
    pub credentials: Option<Arc<dyn CredentialResolver>>,
    /// Longest Wait handled in-process; longer ones need the durable wait (P4.4).
    pub max_inline_wait: Duration,
}

impl Default for NodeHooks {
    fn default() -> Self {
        Self {
            sub_workflow: None,
            response: None,
            credentials: None,
            max_inline_wait: Duration::from_secs(65),
        }
    }
}

// --------------------------------------------------------------------- scratch

/// Per-execution mutable state keyed by node name (Loop Over Items keeps its queue here).
/// Not part of workflow static data: it dies with the execution.
#[derive(Debug, Clone, Default)]
pub struct ExecScratch(Arc<Mutex<BTreeMap<String, Value>>>);

impl ExecScratch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, node: &str) -> Option<Value> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(node)
            .cloned()
    }

    pub fn set(&self, node: &str, v: Value) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(node.to_owned(), v);
    }

    pub fn clear(&self, node: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(node);
    }
}
