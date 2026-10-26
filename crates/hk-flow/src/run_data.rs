//! Execution data: n8n-shaped `runData`, workflow static data, and the persistence trait.
//!
//! Shapes follow the public n8n execution JSON (`data.resultData.runData`). Execution
//! data must never hold secrets: nodes receive credential *references* only, and the
//! engine records items + error messages, never node parameters or credential values.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::FlowError;
use crate::item::Item;

/// Node name -> one entry per run (loops and multi-arrival nodes run more than once).
pub type RunData = IndexMap<String, Vec<NodeRun>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunStatus {
    Running,
    #[default]
    Success,
    Error,
    /// Suspended in a durable Wait; resumes via `Executor::resume`.
    Waiting,
    /// Stopped by the user (n8n `canceled`).
    Canceled,
}

/// Where a run's input came from (n8n `source`). `previousNodeOutput` / `previousNodeRun`
/// are omitted when 0, like n8n does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSource {
    pub previous_node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_node_output: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_node_run: Option<usize>,
}

/// Error as stored in run data: message only, no stack, no parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeRunData {
    /// One slot per output; `null` = output never produced (distinct from `[]`).
    #[serde(default)]
    pub main: Vec<Option<Vec<Item>>>,
}

/// One run of one node (n8n `ITaskData`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRun {
    /// Unix ms.
    #[serde(default)]
    pub start_time: u64,
    #[serde(default)]
    pub execution_time: u64,
    /// Global order in which runs started within the execution.
    #[serde(default)]
    pub execution_index: usize,
    /// One entry per input port.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source: Vec<Option<RunSource>>,
    #[serde(default)]
    pub data: NodeRunData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RunError>,
    #[serde(default)]
    pub execution_status: RunStatus,
}

impl NodeRun {
    /// Items on output `idx`; empty when the output was not produced.
    pub fn output(&self, idx: usize) -> &[Item] {
        self.data
            .main
            .get(idx)
            .and_then(|s| s.as_deref())
            .unwrap_or(&[])
    }
}

// ------------------------------------------------------------ shared run data

/// Live view of the run data of the execution in progress. Nodes (and later the
/// expression engine: `$('Node').item`, `$node[...]`) read previous results through it.
#[derive(Debug, Clone, Default)]
pub struct SharedRunData(Arc<RwLock<RunData>>);

impl SharedRunData {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild from a persisted snapshot (durable-wait resume).
    pub fn from_snapshot(data: RunData) -> Self {
        Self(Arc::new(RwLock::new(data)))
    }

    pub fn snapshot(&self) -> RunData {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Most recent run of `node`.
    pub fn last_run(&self, node: &str) -> Option<NodeRun> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(node)
            .and_then(|r| r.last().cloned())
    }

    pub fn run(&self, node: &str, run_index: usize) -> Option<NodeRun> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(node)
            .and_then(|r| r.get(run_index).cloned())
    }

    pub fn run_count(&self, node: &str) -> usize {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(node)
            .map_or(0, Vec::len)
    }

    pub(crate) fn push(&self, node: &str, run: NodeRun) {
        self.0
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(node.to_owned())
            .or_default()
            .push(run);
    }
}

// ---------------------------------------------------------------- static data

/// Workflow static data (`getWorkflowStaticData`): JSON that survives between executions.
/// Scopes are `global` and `node:<Node Name>`, stored flat like n8n's `staticData` field.
/// The executor returns the updated value; the caller persists it.
#[derive(Debug, Clone, Default)]
pub struct StaticData(Arc<Mutex<StaticInner>>);

#[derive(Debug, Default)]
struct StaticInner {
    scopes: Map<String, Value>,
    dirty: bool,
}

pub const GLOBAL_SCOPE: &str = "global";

pub fn node_scope(node: &str) -> String {
    format!("node:{node}")
}

impl StaticData {
    /// Non-object input (null, absent) starts empty.
    pub fn from_value(v: Option<&Value>) -> Self {
        let scopes = match v {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        Self(Arc::new(Mutex::new(StaticInner {
            scopes,
            dirty: false,
        })))
    }

    /// Like [`from_value`](Self::from_value) but keeps the dirty flag, so a resumed
    /// execution still reports static data written before it was suspended.
    pub fn restore(v: Option<&Value>, dirty: bool) -> Self {
        let s = Self::from_value(v);
        s.lock().dirty = dirty;
        s
    }

    fn lock(&self) -> MutexGuard<'_, StaticInner> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Copy of a scope; `{}` when never written.
    pub fn get(&self, scope: &str) -> Value {
        self.lock()
            .scopes
            .get(scope)
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()))
    }

    pub fn set(&self, scope: &str, v: Value) {
        let mut g = self.lock();
        g.scopes.insert(scope.to_owned(), v);
        g.dirty = true;
    }

    /// Mutate a scope in place (created as `{}`); marks the data changed.
    pub fn update<R>(&self, scope: &str, f: impl FnOnce(&mut Value) -> R) -> R {
        let mut g = self.lock();
        g.dirty = true;
        let slot = g
            .scopes
            .entry(scope.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        f(slot)
    }

    pub fn is_dirty(&self) -> bool {
        self.lock().dirty
    }

    pub fn snapshot(&self) -> Value {
        Value::Object(self.lock().scopes.clone())
    }
}

// ------------------------------------------------------------ persistence

/// What a store keeps per execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    pub status: RunStatus,
    pub started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    pub run_data: RunData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RunError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_node_executed: Option<String>,
}

/// Persistence seam. The executor saves a `Running` record, checkpoints after every node
/// run, then saves the final record, so a crashed execution leaves its progress behind
/// (durable waits build on this). In-memory impl below; sqlite impl lands with hk-store.
#[async_trait]
pub trait RunStore: Send + Sync + std::fmt::Debug {
    /// Insert or replace by `record.id`.
    async fn save(&self, record: &ExecutionRecord) -> Result<(), FlowError>;
    async fn load(&self, id: &str) -> Result<Option<ExecutionRecord>, FlowError>;
    /// Newest first (by `started_at`, ties by insertion order reversed).
    async fn list(&self, workflow_id: Option<&str>) -> Result<Vec<ExecutionRecord>, FlowError>;
    async fn delete(&self, id: &str) -> Result<bool, FlowError>;
}

#[derive(Debug, Default)]
pub struct InMemoryRunStore {
    inner: Mutex<IndexMap<String, ExecutionRecord>>,
}

impl InMemoryRunStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, IndexMap<String, ExecutionRecord>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl RunStore for InMemoryRunStore {
    async fn save(&self, record: &ExecutionRecord) -> Result<(), FlowError> {
        self.lock().insert(record.id.clone(), record.clone());
        Ok(())
    }

    async fn load(&self, id: &str) -> Result<Option<ExecutionRecord>, FlowError> {
        Ok(self.lock().get(id).cloned())
    }

    async fn list(&self, workflow_id: Option<&str>) -> Result<Vec<ExecutionRecord>, FlowError> {
        let mut out: Vec<ExecutionRecord> = self
            .lock()
            .values()
            .filter(|r| workflow_id.is_none() || r.workflow_id.as_deref() == workflow_id)
            .cloned()
            .collect();
        out.reverse();
        out.sort_by_key(|r| std::cmp::Reverse(r.started_at));
        Ok(out)
    }

    async fn delete(&self, id: &str) -> Result<bool, FlowError> {
        Ok(self.lock().shift_remove(id).is_some())
    }
}
