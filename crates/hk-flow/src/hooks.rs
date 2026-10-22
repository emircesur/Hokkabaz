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

// ---------------------------------------------------------------- remote nodes

/// Runs a node this engine has no implementation for somewhere that does (a configured n8n
/// instance). Only consulted when the registry has no `(type, version)`; the node's
/// credentials/parameters travel with it, expressions that reach other nodes will not resolve.
#[async_trait]
pub trait RemoteNodeRunner: Send + Sync + fmt::Debug {
    async fn run_node(
        &self,
        node: &crate::workflow::Node,
        input: &[crate::item::Item],
    ) -> Result<Vec<Vec<crate::item::Item>>, FlowError>;
}

// ----------------------------------------------------------------------- bundle

#[derive(Debug, Clone)]
pub struct NodeHooks {
    pub sub_workflow: Option<Arc<dyn SubWorkflowRunner>>,
    pub response: Option<Arc<dyn ResponseSink>>,
    pub credentials: Option<Arc<dyn CredentialResolver>>,
    /// Fallback for unsupported node types; `None` fails them as before.
    pub remote_nodes: Option<Arc<dyn RemoteNodeRunner>>,
    /// Longest Wait handled in-process; longer ones suspend the execution (durable wait).
    pub max_inline_wait: Duration,
    /// The executor can park a suspended execution (it has a `WaitStore`). Set by the
    /// executor; nodes must not suspend when this is false.
    pub durable_wait: bool,
    /// Builds chat models for the `lmChat*` nodes. `None` = hk-llm providers built straight
    /// from the node's credential ([`HkLlmChatModelFactory`](crate::nodes::ai::HkLlmChatModelFactory)).
    #[cfg(feature = "ai")]
    pub chat_models: Option<Arc<dyn crate::nodes::ai::ChatModelFactory>>,
    /// Runs the full hk-agent loop for `hokkabaz.agent`; the server/FFI supplies it.
    #[cfg(feature = "ai")]
    pub agent_runner: Option<Arc<dyn crate::nodes::ai::AgentRunner>>,
    /// Chat history behind `memoryBufferWindow`.
    #[cfg(feature = "ai")]
    pub chat_memory: crate::nodes::ai::ChatMemoryStore,
}

impl Default for NodeHooks {
    fn default() -> Self {
        Self {
            sub_workflow: None,
            response: None,
            credentials: None,
            remote_nodes: None,
            max_inline_wait: Duration::from_secs(65),
            durable_wait: false,
            #[cfg(feature = "ai")]
            chat_models: None,
            #[cfg(feature = "ai")]
            agent_runner: None,
            #[cfg(feature = "ai")]
            chat_memory: crate::nodes::ai::ChatMemoryStore::new(),
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

    /// Copy of all entries; persisted with a suspended execution.
    pub fn snapshot(&self) -> BTreeMap<String, Value> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn restore(entries: BTreeMap<String, Value>) -> Self {
        Self(Arc::new(Mutex::new(entries)))
    }

    pub fn clear(&self, node: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(node);
    }
}
