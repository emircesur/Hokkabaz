//! Workflow executor: n8n execution semantics on top of the model in `workflow.rs`.
//!
//! Clean-room: behavior taken from the public n8n docs (execution order, node settings,
//! error handling, pinned data, static data), not from n8n source.
//!
//! # Execution order (exact rule implemented)
//!
//! The run starts at the start node (trigger): it is not executed, its output is the
//! supplied items (or its pinned items, see below). After every node run:
//!
//! 1. Walk its outputs in ascending output index; walk each output's connections in
//!    stored order. Only `main` connections carry items. An output with zero items
//!    feeds nothing, so a branch that produced no data never runs (no run entry).
//! 2. A target with one connected input becomes *ready* per arrival. A target with
//!    several connected input ports (Merge) collects one arrival per port in a waiting
//!    entry and becomes ready once every connected port has data. Two arrivals on the
//!    same port start a second waiting entry (second run).
//! 3. **`v1`** (`settings.executionOrder == "v1"`): the ready nodes are sorted by canvas
//!    position, top to bottom (`y` ascending), ties left to right (`x` ascending), stable
//!    otherwise, and pushed on the *front* of the work stack in that order. The first
//!    one runs next and everything it unlocks is pushed in front of its siblings, so a
//!    branch runs to completion (depth-first) before the next branch starts.
//! 4. **`v0`** (setting absent or any other value): ready nodes are appended to the
//!    *back* of a FIFO queue in output/connection order (breadth-first). Simplified
//!    legacy mode; kept so old exports still run, not tuned for exact legacy parity.
//! 5. When the work stack is empty but waiting entries remain (a Merge whose other
//!    branch produced nothing), the earliest-created waiting entry runs with the inputs
//!    it has; missing ports are empty. This is the v1 "run with partial inputs" rule.
//!
//! # Node settings
//!
//! - `disabled`: not executed, no run entry; `input[0]` is forwarded on output 0.
//! - pinned data (only when [`ExecutorConfig::use_pin_data`], i.e. manual runs): the node is
//!   not executed; its pinned items are output 0 and a normal run entry is recorded. A
//!   pinned start node replaces the supplied trigger items.
//! - `executeOnce`: the node sees only the first item of input port 0.
//! - `retryOnFail`: `maxTries` is the total number of attempts (default 3, clamped 1..=5),
//!   `waitBetweenTries` ms between attempts (default 1000, clamped to 5000). Only a
//!   returned error retries; retries finish before `onError` is applied.
//! - `alwaysOutputData`: when every output is empty, output 0 becomes one empty item `{}`.
//! - `onError` (legacy `continueOnFail` = `continueRegularOutput`):
//!   - `stopWorkflow` (default): the run entry records the error, the execution ends with
//!     status `error`, nothing further runs.
//!   - `continueRegularOutput`: a thrown error becomes output 0 = one `{"error": msg}` item
//!     per input item (one item if the input was empty). Items a node returns itself are
//!     left alone.
//!   - `continueErrorOutput`: the node gets one extra output after its declared main
//!     outputs. A thrown error sends every input item there as `{...input json, "error": msg}`.
//!     When the node returns normally, items that carry the per-item `error` marker
//!     ([`Item::error`]) are moved from the regular outputs to the error output.
//!
//!   In both continue modes the run entry is a success; the error lives in the items.
//!
//! `pairedItem` is filled for output items lacking it: input of exactly one item pairs
//! everything to item 0, equal lengths pair by index (what n8n auto-assigns).
//!
//! # Expressions
//!
//! Every node run gets an [`ExprEnv`] built once per execution: the raw parameters of all
//! workflow nodes (`$('X').params`), `$execution.mode`, the workflow timezone
//! (`settings.timezone`, else UTC), `$env` restricted to [`ExecutorConfig::env_allowlist`]
//! (empty by default: host env can hold secrets) and `$vars`. When the execution ends the
//! evaluator's per-execution realm is released.
//!
//! # Durable waits
//!
//! With a [`WaitStore`] attached ([`Executor::with_wait_store`]) a Wait node that cannot be
//! held in-process returns [`FlowError::Suspend`]. The executor then stops scheduling,
//! snapshots everything needed to continue ([`WaitSnapshot`]: workflow, run data, pending
//! work stack, join buffers, scratch, static data), parks it in the store, checkpoints
//! the execution as `waiting` and returns `status == Waiting`. [`Executor::resume`]
//! rebuilds the run from a snapshot and continues: the Wait node's output is its input
//! (timer) or the webhook item that resumed it. Nothing else re-runs.
//!
//! Sub-nodes (`ai_*` connections) are not scheduled: only `main` connections carry items.
//! A root node pulls them through [`NodeContext::sub_nodes`](crate::node::NodeContext::sub_nodes).
//!
//! Not here yet: cancellation, workflow timeout, error
//! workflows (see `triggers::LocalTriggerHost::on_execution_finished`).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::FlowError;
use crate::expr::{ExprEnv, ExpressionEvaluator};
use crate::hooks::{ExecScratch, NodeHooks};
use crate::item::{Item, PairedItem};
use crate::node::{FlowNode, NodeContext, NodeOutput, NodeRegistry, SubNodeEnv};
use crate::run_data::{
    ExecutionRecord, NodeRun, NodeRunData, RunError, RunSource, RunStatus, RunStore, SharedRunData,
    StaticData,
};
use crate::triggers::wait::{WaitRecord, WaitRequest, WaitStore, WaitTrigger};
use crate::workflow::{ErrorMode, MAIN, Node, Workflow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionOrder {
    V0,
    V1,
}

impl ExecutionOrder {
    pub fn of(wf: &Workflow) -> Self {
        match wf
            .settings
            .as_ref()
            .and_then(|s| s.execution_order.as_deref())
        {
            Some("v1") => Self::V1,
            _ => Self::V0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Use `pinData` instead of executing pinned nodes. n8n does this for manual runs only.
    pub use_pin_data: bool,
    /// Loop guard: node runs per execution before it is stopped with an error.
    pub max_node_runs: usize,
    /// `$execution.mode`: `manual`, `trigger`, `webhook`, ...
    pub mode: String,
    /// Host env var names readable through `$env`. Empty by default.
    pub env_allowlist: Vec<String>,
    /// `$vars`.
    pub vars: Map<String, Value>,
    /// Expression engine; `None` = process-wide QuickJS default.
    pub evaluator: Option<Arc<dyn ExpressionEvaluator>>,
    /// Freeze `$now` (unix ms); tests.
    pub frozen_now_ms: Option<i64>,
    /// Sub-workflow runner, webhook response sink, credential resolver for nodes.
    pub hooks: NodeHooks,
    /// Public base of the webhook listener (`https://host:5678`). Enables
    /// `$execution.resumeUrl` (`<base>/webhook-waiting/<execution id>`).
    pub webhook_base_url: Option<String>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            use_pin_data: false,
            max_node_runs: 10_000,
            mode: "manual".into(),
            env_allowlist: Vec::new(),
            vars: Map::new(),
            evaluator: None,
            frozen_now_ms: None,
            hooks: NodeHooks::default(),
            webhook_base_url: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecuteInput {
    /// Node that emits `start_items` (trigger). Not executed.
    pub start_node: String,
    pub start_items: Vec<Item>,
    /// Defaults to a process-unique `exec-<n>`.
    pub execution_id: Option<String>,
}

impl ExecuteInput {
    pub fn new(start_node: impl Into<String>, start_items: Vec<Item>) -> Self {
        Self {
            start_node: start_node.into(),
            start_items,
            execution_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub execution_id: String,
    pub status: RunStatus,
    pub run_data: crate::run_data::RunData,
    /// Set when `status == Error`.
    pub error: Option<RunError>,
    pub last_node_executed: Option<String>,
    /// Updated workflow static data; `None` when no node touched it.
    pub static_data: Option<Value>,
    /// Set when `status == Waiting`: what to persist/arm. Already put in the executor's
    /// `WaitStore` when one is attached.
    pub wait: Option<WaitRecord>,
}

/// Why a suspended execution continues.
#[derive(Debug, Clone, PartialEq)]
pub enum ResumeReason {
    /// Timer fired (or the timeout of a webhook wait): Wait forwards its input items.
    Elapsed,
    /// Webhook/form call: Wait outputs these items instead of its input.
    Webhook(Vec<Item>),
}

/// Everything needed to continue a suspended execution after a restart. JSON-serializable;
/// holds items and error messages only, never credential values (nodes only ever hold
/// credential references).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitSnapshot {
    pub execution_id: String,
    /// The workflow as it was when the run started. Replace it before `resume` to continue
    /// on an edited version (n8n resumes on the current one).
    pub workflow: Workflow,
    pub started_at: u64,
    pub suspended_at: u64,
    pub run_data: crate::run_data::RunData,
    pub next_index: usize,
    #[serde(default)]
    pub last_node: Option<String>,
    #[serde(default)]
    pub static_data: Value,
    #[serde(default)]
    pub static_dirty: bool,
    #[serde(default)]
    pub scratch: BTreeMap<String, Value>,
    /// Ready work not yet run, front first.
    #[serde(default)]
    pub stack: Vec<Task>,
    /// Join buffers (Merge) still waiting for another branch.
    #[serde(default)]
    pub waiting: IndexMap<String, Vec<Waiting>>,
    /// The Wait node's own pending run.
    pub suspended: Task,
    pub request: WaitRequest,
}

#[derive(Debug, Clone)]
pub struct Executor {
    registry: Arc<NodeRegistry>,
    store: Option<Arc<dyn RunStore>>,
    wait_store: Option<Arc<dyn WaitStore>>,
    config: ExecutorConfig,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl Executor {
    pub fn new(registry: Arc<NodeRegistry>) -> Self {
        Self {
            registry,
            store: None,
            wait_store: None,
            config: ExecutorConfig::default(),
        }
    }

    /// Enables durable waits: suspended executions are parked here.
    pub fn with_wait_store(mut self, store: Arc<dyn WaitStore>) -> Self {
        self.wait_store = Some(store);
        self
    }

    pub fn wait_store(&self) -> Option<&Arc<dyn WaitStore>> {
        self.wait_store.as_ref()
    }

    pub fn with_store(mut self, store: Arc<dyn RunStore>) -> Self {
        self.store = Some(store);
        self
    }

    pub fn with_config(mut self, config: ExecutorConfig) -> Self {
        self.config = config;
        self
    }

    /// Runs the workflow to completion. `Err` = could not start or the store failed;
    /// node failures are reported in the result (`status == Error`), not as `Err`.
    pub async fn execute(
        &self,
        wf: &Workflow,
        input: &ExecuteInput,
    ) -> Result<ExecutionResult, FlowError> {
        if wf.node(&input.start_node).is_none() {
            return Err(FlowError::Invalid(format!(
                "start node `{}` not in workflow",
                input.start_node
            )));
        }
        let id = input
            .execution_id
            .clone()
            .unwrap_or_else(|| format!("exec-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed)));
        let mut run = Run {
            exec: self,
            wf,
            order: ExecutionOrder::of(wf),
            id: id.clone(),
            expr: self.expr_env(wf, &id),
            scratch: ExecScratch::new(),
            started_at: now_ms(),
            run_data: SharedRunData::new(),
            static_data: StaticData::from_value(wf.static_data.as_ref()),
            stack: VecDeque::new(),
            waiting: IndexMap::new(),
            layouts: HashMap::new(),
            next_index: 0,
            last_node: None,
            sub: SubNodeEnv::build(&self.registry, wf),
        };
        let res = run.drive(input).await;
        self.release_expr(&id);
        res
    }

    /// Continue a suspended execution. `Err` = could not continue (store failure);
    /// node failures land in the result as in [`execute`](Self::execute). The result can
    /// be `Waiting` again (a second Wait later in the workflow).
    pub async fn resume(
        &self,
        snapshot: WaitSnapshot,
        reason: ResumeReason,
    ) -> Result<ExecutionResult, FlowError> {
        let WaitSnapshot {
            execution_id: id,
            workflow: wf,
            started_at,
            suspended_at,
            run_data,
            next_index,
            last_node,
            static_data,
            static_dirty,
            scratch,
            stack,
            waiting,
            suspended,
            request: _,
        } = snapshot;
        let mut run = Run {
            exec: self,
            wf: &wf,
            order: ExecutionOrder::of(&wf),
            id: id.clone(),
            expr: self.expr_env(&wf, &id),
            scratch: ExecScratch::restore(scratch),
            started_at,
            run_data: SharedRunData::from_snapshot(run_data),
            static_data: StaticData::restore(Some(&static_data), static_dirty),
            stack: stack.into(),
            waiting,
            layouts: HashMap::new(),
            next_index,
            last_node,
            sub: SubNodeEnv::build(&self.registry, &wf),
        };
        let res = run
            .continue_after_wait(suspended, suspended_at, reason)
            .await;
        self.release_expr(&id);
        res
    }

    /// Resume the parked execution `execution_id`. `Ok(None)` = nothing parked under that
    /// id (never suspended, already resumed or claimed by another worker).
    pub async fn resume_from_store(
        &self,
        execution_id: &str,
        reason: ResumeReason,
    ) -> Result<Option<ExecutionResult>, FlowError> {
        let store = self.require_wait_store()?;
        let Some(rec) = store.take(execution_id).await? else {
            return Ok(None);
        };
        self.resume(rec.snapshot, reason).await.map(Some)
    }

    /// Resume a parked execution because a call hit its resume URL. Checks the HTTP method
    /// and suffix the Wait node was configured with; `Err(Invalid)` on mismatch (the
    /// execution stays parked), `Ok(None)` when nothing is parked under the id.
    pub async fn resume_webhook(
        &self,
        execution_id: &str,
        method: &str,
        suffix: Option<&str>,
        items: Vec<Item>,
    ) -> Result<Option<ExecutionResult>, FlowError> {
        let store = self.require_wait_store()?;
        let Some(rec) = store.get(execution_id).await? else {
            return Ok(None);
        };
        match &rec.request.on {
            WaitTrigger::Webhook {
                http_method,
                suffix: want,
            } => {
                if !http_method.eq_ignore_ascii_case(method)
                    || want.as_deref().unwrap_or("") != suffix.unwrap_or("")
                {
                    return Err(FlowError::Invalid(format!(
                        "execution {execution_id} waits for {http_method} /{}",
                        want.as_deref().unwrap_or("")
                    )));
                }
            }
            WaitTrigger::Form => {}
            WaitTrigger::Time => {
                return Err(FlowError::Invalid(format!(
                    "execution {execution_id} waits for a timer, not a webhook"
                )));
            }
        }
        self.resume_from_store(execution_id, ResumeReason::Webhook(items))
            .await
    }

    /// Resume every parked execution whose timer is due at `now_ms` (also the timeout of
    /// webhook waits). Call after a restart and from the scheduler tick; overdue waits
    /// resume immediately. One result per claimed execution.
    pub async fn resume_due(&self, now_ms: i64) -> Vec<Result<ExecutionResult, FlowError>> {
        let Some(store) = &self.wait_store else {
            return Vec::new();
        };
        let due = match store.due(now_ms).await {
            Ok(d) => d,
            Err(e) => return vec![Err(e)],
        };
        let mut out = Vec::new();
        for rec in due {
            // `take` is the claim: a concurrent worker that got it first makes this a no-op
            match store.take(&rec.execution_id).await {
                Ok(Some(rec)) => out.push(self.resume(rec.snapshot, ResumeReason::Elapsed).await),
                Ok(None) => {}
                Err(e) => out.push(Err(e)),
            }
        }
        out
    }

    fn require_wait_store(&self) -> Result<&Arc<dyn WaitStore>, FlowError> {
        self.wait_store
            .as_ref()
            .ok_or_else(|| FlowError::Invalid("executor has no wait store".into()))
    }

    fn expr_env(&self, wf: &Workflow, execution_id: &str) -> ExprEnv {
        let c = &self.config;
        let env: BTreeMap<String, String> = c
            .env_allowlist
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| (k.clone(), v)))
            .collect();
        let node_params: BTreeMap<String, Value> = wf
            .nodes
            .iter()
            .map(|n| (n.name.clone(), Value::Object(n.parameters.clone())))
            .collect();
        ExprEnv {
            evaluator: c.evaluator.clone(),
            env: Arc::new(env),
            vars: Arc::new(c.vars.clone()),
            node_params: Arc::new(node_params),
            mode: Some(c.mode.clone()),
            workflow_active: wf.active.unwrap_or(false),
            resume_url: c
                .webhook_base_url
                .as_ref()
                .map(|b| format!("{}/webhook-waiting/{execution_id}", b.trim_end_matches('/'))),
            timezone: wf.settings.as_ref().and_then(|s| s.timezone.clone()),
            now_ms: c.frozen_now_ms,
        }
    }

    fn release_expr(&self, execution_id: &str) {
        match &self.config.evaluator {
            Some(e) => e.release(execution_id),
            #[cfg(feature = "expr")]
            None => crate::expr::release_default(execution_id),
            #[cfg(not(feature = "expr"))]
            None => {}
        }
    }
}

// ------------------------------------------------------------------- state

/// Ready work: a node plus the items on each input port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub node: String,
    pub input: Vec<Vec<Item>>,
    pub source: Vec<Option<RunSource>>,
}

/// Join buffer entry of a node with several connected inputs (Merge).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Waiting {
    pub slots: Vec<Option<Vec<Item>>>,
    pub source: Vec<Option<RunSource>>,
}

impl Waiting {
    fn new(len: usize) -> Self {
        Self {
            slots: vec![None; len],
            source: vec![None; len],
        }
    }

    fn into_task(self, node: String) -> Task {
        Task {
            node,
            input: self
                .slots
                .into_iter()
                .map(Option::unwrap_or_default)
                .collect(),
            source: self.source,
        }
    }
}

struct Run<'a> {
    exec: &'a Executor,
    wf: &'a Workflow,
    order: ExecutionOrder,
    id: String,
    started_at: u64,
    expr: ExprEnv,
    scratch: ExecScratch,
    run_data: SharedRunData,
    static_data: StaticData,
    stack: VecDeque<Task>,
    waiting: IndexMap<String, Vec<Waiting>>,
    /// node -> (input port count, connected input ports)
    layouts: HashMap<String, (usize, usize)>,
    next_index: usize,
    last_node: Option<String>,
    /// `ai_*` sub-node wiring, handed to every node context.
    sub: Option<Arc<SubNodeEnv>>,
}

enum Flow {
    Continue,
    Stop(RunError),
    Suspend(Box<Task>, Box<WaitRequest>),
}

impl<'a> Run<'a> {
    async fn drive(&mut self, input: &ExecuteInput) -> Result<ExecutionResult, FlowError> {
        self.checkpoint(RunStatus::Running, None, None).await?;

        let start = input.start_node.as_str();
        let items = match self.pinned(start) {
            Some(p) => p,
            None => input.start_items.clone(),
        };
        let started = now_ms();
        let out = vec![items];
        let idx = self.record(start, Vec::new(), started, Instant::now(), &out, None);
        self.checkpoint(RunStatus::Running, None, None).await?;
        self.propagate(start, idx, &out);
        self.run_loop().await
    }

    /// Continue after a durable wait: the Wait node completes now, then the loop goes on.
    async fn continue_after_wait(
        &mut self,
        task: Task,
        suspended_at: u64,
        reason: ResumeReason,
    ) -> Result<ExecutionResult, FlowError> {
        let first_input = task.input.first().cloned().unwrap_or_default();
        let mut out = vec![match reason {
            ResumeReason::Elapsed => first_input.clone(),
            ResumeReason::Webhook(items) => items,
        }];
        assign_paired(&mut out, &first_input);
        self.checkpoint(RunStatus::Running, None, None).await?;
        let idx = self.record(
            &task.node,
            task.source,
            suspended_at,
            Instant::now(),
            &out,
            None,
        );
        self.checkpoint(RunStatus::Running, None, None).await?;
        self.propagate(&task.node, idx, &out);
        self.run_loop().await
    }

    async fn run_loop(&mut self) -> Result<ExecutionResult, FlowError> {
        let mut runs = 0usize;
        let mut failure = None;
        loop {
            let task = match self.stack.pop_front() {
                Some(t) => t,
                None => match self.take_waiting() {
                    Some(t) => t,
                    None => break,
                },
            };
            runs += 1;
            if runs > self.exec.config.max_node_runs {
                failure = Some(RunError {
                    message: format!(
                        "execution stopped after {} node runs (loop guard)",
                        self.exec.config.max_node_runs
                    ),
                    node: Some(task.node),
                });
                break;
            }
            match self.run_task(task).await? {
                Flow::Continue => {}
                Flow::Stop(e) => {
                    failure = Some(e);
                    break;
                }
                Flow::Suspend(task, req) => return self.suspend(*task, *req).await,
            }
        }

        let status = if failure.is_some() {
            RunStatus::Error
        } else {
            RunStatus::Success
        };
        self.checkpoint(status, failure.clone(), Some(now_ms()))
            .await?;
        Ok(ExecutionResult {
            execution_id: self.id.clone(),
            status,
            run_data: self.run_data.snapshot(),
            error: failure,
            last_node_executed: self.last_node.clone(),
            static_data: self
                .static_data
                .is_dirty()
                .then(|| self.static_data.snapshot()),
            wait: None,
        })
    }

    /// Park the execution: snapshot, persist, report `Waiting`.
    async fn suspend(
        &mut self,
        task: Task,
        request: WaitRequest,
    ) -> Result<ExecutionResult, FlowError> {
        let snapshot = WaitSnapshot {
            execution_id: self.id.clone(),
            workflow: self.wf.clone(),
            started_at: self.started_at,
            suspended_at: now_ms(),
            run_data: self.run_data.snapshot(),
            next_index: self.next_index,
            last_node: self.last_node.clone(),
            static_data: self.static_data.snapshot(),
            static_dirty: self.static_data.is_dirty(),
            scratch: self.scratch.snapshot(),
            stack: self.stack.drain(..).collect(),
            waiting: std::mem::take(&mut self.waiting),
            suspended: task,
            request: request.clone(),
        };
        let record = WaitRecord {
            execution_id: self.id.clone(),
            workflow_id: self.wf.id.clone(),
            request,
            snapshot,
        };
        if let Some(ws) = &self.exec.wait_store {
            ws.put(&record).await?;
        }
        self.checkpoint(RunStatus::Waiting, None, None).await?;
        Ok(ExecutionResult {
            execution_id: self.id.clone(),
            status: RunStatus::Waiting,
            run_data: self.run_data.snapshot(),
            error: None,
            last_node_executed: self.last_node.clone(),
            static_data: self
                .static_data
                .is_dirty()
                .then(|| self.static_data.snapshot()),
            wait: Some(record),
        })
    }

    fn pinned(&self, node: &str) -> Option<Vec<Item>> {
        if self.exec.config.use_pin_data {
            self.wf.pinned_items(node)
        } else {
            None
        }
    }

    async fn checkpoint(
        &self,
        status: RunStatus,
        error: Option<RunError>,
        finished_at: Option<u64>,
    ) -> Result<(), FlowError> {
        let Some(store) = &self.exec.store else {
            return Ok(());
        };
        store
            .save(&ExecutionRecord {
                id: self.id.clone(),
                workflow_id: self.wf.id.clone(),
                status,
                started_at: self.started_at,
                finished_at,
                run_data: self.run_data.snapshot(),
                error,
                last_node_executed: self.last_node.clone(),
            })
            .await
    }

    /// Append a run entry; returns its index within the node's runs.
    fn record(
        &mut self,
        node: &str,
        source: Vec<Option<RunSource>>,
        started: u64,
        t0: Instant,
        out: &NodeOutput,
        error: Option<RunError>,
    ) -> usize {
        let run_index = self.run_data.run_count(node);
        self.run_data.push(
            node,
            NodeRun {
                start_time: started,
                execution_time: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
                execution_index: self.next_index,
                source,
                data: NodeRunData {
                    main: out.iter().map(|o| Some(o.clone())).collect(),
                },
                execution_status: if error.is_some() {
                    RunStatus::Error
                } else {
                    RunStatus::Success
                },
                error,
            },
        );
        self.next_index += 1;
        self.last_node = Some(node.to_owned());
        run_index
    }

    // ----------------------------------------------------------- scheduling

    /// (input port count, connected input ports) for the main inputs of `node`.
    fn layout(&mut self, node: &str) -> (usize, usize) {
        if let Some(l) = self.layouts.get(node) {
            return *l;
        }
        let mut ports: Vec<u32> = self
            .wf
            .incoming(node)
            .into_iter()
            .filter(|e| e.conn_type == MAIN && e.target_type == MAIN)
            .map(|e| e.input)
            .collect();
        ports.sort_unstable();
        ports.dedup();
        let len = ports.last().map_or(1, |m| *m as usize + 1);
        let l = (len, ports.len().max(1));
        self.layouts.insert(node.to_owned(), l);
        l
    }

    fn arrive(
        &mut self,
        target: &str,
        port: usize,
        items: Vec<Item>,
        src: RunSource,
        ready: &mut Vec<Task>,
    ) {
        let (len, required) = self.layout(target);
        let port = port.min(len - 1);
        if required <= 1 {
            let mut input = vec![Vec::new(); len];
            input[port] = items;
            let mut source = vec![None; len];
            source[port] = Some(src);
            ready.push(Task {
                node: target.to_owned(),
                input,
                source,
            });
            return;
        }
        let entries = self.waiting.entry(target.to_owned()).or_default();
        let pos = match entries.iter().position(|w| w.slots[port].is_none()) {
            Some(p) => p,
            None => {
                entries.push(Waiting::new(len));
                entries.len() - 1
            }
        };
        entries[pos].slots[port] = Some(items);
        entries[pos].source[port] = Some(src);
        let filled = entries[pos].slots.iter().flatten().count();
        if filled >= required {
            let w = entries.remove(pos);
            if entries.is_empty() {
                self.waiting.shift_remove(target);
            }
            ready.push(w.into_task(target.to_owned()));
        }
    }

    /// Queue everything `from` unlocked, in the configured execution order.
    fn propagate(&mut self, from: &str, run_index: usize, out: &NodeOutput) {
        let wf = self.wf;
        let mut ready = Vec::new();
        for (oi, items) in out.iter().enumerate() {
            if items.is_empty() {
                continue;
            }
            for t in wf.destinations(from, MAIN, oi) {
                if t.conn_type != MAIN || wf.node(&t.node).is_none() {
                    continue;
                }
                let src = RunSource {
                    previous_node: from.to_owned(),
                    previous_node_output: (oi > 0).then_some(oi),
                    previous_node_run: (run_index > 0).then_some(run_index),
                };
                self.arrive(&t.node, t.index as usize, items.clone(), src, &mut ready);
            }
        }
        match self.order {
            ExecutionOrder::V1 => {
                ready.sort_by(|a, b| {
                    let pos = |n: &str| wf.node(n).map_or((0.0, 0.0), |n| (n.y(), n.x()));
                    let (ay, ax) = pos(&a.node);
                    let (by, bx) = pos(&b.node);
                    ay.total_cmp(&by).then(ax.total_cmp(&bx))
                });
                for t in ready.into_iter().rev() {
                    self.stack.push_front(t);
                }
            }
            ExecutionOrder::V0 => self.stack.extend(ready),
        }
    }

    /// Stack empty: release the earliest waiting entry with the inputs it has.
    fn take_waiting(&mut self) -> Option<Task> {
        let name = self.waiting.keys().next()?.clone();
        let entries = self.waiting.get_mut(&name)?;
        let w = entries.remove(0);
        if entries.is_empty() {
            self.waiting.shift_remove(&name);
        }
        Some(w.into_task(name))
    }

    // ------------------------------------------------------------ node runs

    async fn run_task(&mut self, task: Task) -> Result<Flow, FlowError> {
        let wf = self.wf;
        let Some(node) = wf.node(&task.node) else {
            return Ok(Flow::Continue);
        };

        if node.is_disabled() {
            let pass = task.input.first().cloned().unwrap_or_default();
            self.propagate(&node.name, 0, &vec![pass]);
            return Ok(Flow::Continue);
        }

        let started = now_ms();
        let t0 = Instant::now();

        if let Some(items) = self.pinned(&node.name) {
            let out = vec![items];
            let idx = self.record(&node.name, task.source, started, t0, &out, None);
            self.checkpoint(RunStatus::Running, None, None).await?;
            self.propagate(&node.name, idx, &out);
            return Ok(Flow::Continue);
        }

        let mut input = task.input.clone();
        if node.execute_once == Some(true)
            && let Some(p0) = input.first_mut()
        {
            p0.truncate(1);
        }
        let mut ctx = NodeContext::new(node.clone(), input, self.run_data.run_count(&node.name));
        ctx.workflow_id = wf.id.clone();
        ctx.workflow_name = wf.name.clone();
        ctx.execution_id = Some(self.id.clone());
        ctx.source = task.source.clone();
        ctx.run_data = self.run_data.clone();
        ctx.static_data = self.static_data.clone();
        ctx.expr = self.expr.clone();
        ctx.hooks = self.exec.config.hooks.clone();
        ctx.hooks.durable_wait = self.exec.wait_store.is_some();
        ctx.scratch = self.scratch.clone();
        ctx.sub = self.sub.clone();

        let (declared, result) = match self.exec.registry.resolve(node) {
            Ok(imp) => (
                main_output_count(imp.as_ref()),
                call_with_retry(imp, node, &ctx).await,
            ),
            Err(FlowError::UnsupportedNode(key)) => match &ctx.hooks.remote_nodes {
                Some(remote) => (None, remote.run_node(node, ctx.input.first().map(Vec::as_slice).unwrap_or(&[])).await),
                None => (None, Err(FlowError::UnsupportedNode(key))),
            },
            Err(e) => (None, Err(e)),
        };

        let result = match result {
            Err(FlowError::Suspend(req)) => {
                return Ok(Flow::Suspend(Box::new(task), req));
            }
            other => other,
        };

        let mode = node.error_mode();
        let first_input = ctx.input.first().map(Vec::as_slice).unwrap_or(&[]);
        let mut out = match result {
            Ok(mut out) => {
                let n = declared.unwrap_or(1).max(out.len());
                out.resize_with(n, Vec::new);
                assign_paired(&mut out, first_input);
                if mode == ErrorMode::ContinueErrorOutput {
                    let mut errs = Vec::new();
                    for slot in &mut out {
                        let (bad, good): (Vec<_>, Vec<_>) =
                            slot.drain(..).partition(|i| i.error.is_some());
                        *slot = good;
                        errs.extend(bad);
                    }
                    out.push(errs);
                }
                out
            }
            Err(e) => {
                let message = match e {
                    FlowError::Node { message, .. } => message,
                    other => other.to_string(),
                };
                match mode {
                    ErrorMode::ContinueRegularOutput => {
                        vec![error_items(first_input, &message, false)]
                    }
                    ErrorMode::ContinueErrorOutput => {
                        let n = declared.unwrap_or(1);
                        let mut out = vec![Vec::new(); n];
                        out.push(error_items(first_input, &message, true));
                        out
                    }
                    _ => {
                        let err = RunError {
                            message,
                            node: Some(node.name.clone()),
                        };
                        self.record(
                            &node.name,
                            task.source,
                            started,
                            t0,
                            &Vec::new(),
                            Some(err.clone()),
                        );
                        return Ok(Flow::Stop(err));
                    }
                }
            }
        };

        if node.always_output_data == Some(true) && out.iter().all(Vec::is_empty) {
            if out.is_empty() {
                out.push(Vec::new());
            }
            let mut empty = Item::default();
            if !first_input.is_empty() {
                empty.paired_item = Some(PairedItem::Index(0));
            }
            out[0] = vec![empty];
        }

        let idx = self.record(&node.name, task.source, started, t0, &out, None);
        self.checkpoint(RunStatus::Running, None, None).await?;
        self.propagate(&node.name, idx, &out);
        Ok(Flow::Continue)
    }
}

// ----------------------------------------------------------------- helpers

/// `Some(n)` when the node declares a fixed list of outputs; `None` for expression outputs.
fn main_output_count(node: &dyn FlowNode) -> Option<usize> {
    match &node.description().outputs {
        crate::description::IoDecl::List(l) => {
            Some(l.iter().filter(|o| o.conn_type() == MAIN).count().max(1))
        }
        crate::description::IoDecl::Expression(_) => None,
    }
}

async fn call_with_retry(
    imp: &Arc<dyn FlowNode>,
    node: &Node,
    ctx: &NodeContext,
) -> Result<NodeOutput, FlowError> {
    let tries = if node.retry_on_fail == Some(true) {
        node.max_tries.unwrap_or(3).clamp(1, 5)
    } else {
        1
    };
    let wait = node.wait_between_tries.unwrap_or(1000).min(5000);
    let mut attempt = 1;
    loop {
        match imp.execute(ctx).await {
            Ok(o) => return Ok(o),
            Err(e) if attempt >= tries || matches!(e, FlowError::Suspend(_)) => return Err(e),
            Err(_) => {
                attempt += 1;
                if wait > 0 {
                    tokio::time::sleep(Duration::from_millis(wait)).await;
                }
            }
        }
    }
}

fn error_items(input: &[Item], message: &str, keep_json: bool) -> Vec<Item> {
    let make = |base: Option<&Item>, i: usize| {
        let mut json = match (keep_json, base) {
            (true, Some(it)) => it.json.clone(),
            _ => Map::new(),
        };
        json.insert("error".into(), Value::String(message.to_owned()));
        let mut item = Item::new(json);
        if base.is_some() {
            item.paired_item = Some(PairedItem::Index(i));
        }
        item
    };
    if input.is_empty() {
        return vec![make(None, 0)];
    }
    input
        .iter()
        .enumerate()
        .map(|(i, it)| make(Some(it), i))
        .collect()
}

/// n8n auto-pairing: one input item pairs everything to it, equal lengths pair by index.
fn assign_paired(out: &mut NodeOutput, input: &[Item]) {
    for slot in out {
        let same_len = input.len() == slot.len();
        for (i, item) in slot.iter_mut().enumerate() {
            if item.paired_item.is_some() {
                continue;
            }
            if input.len() == 1 {
                item.paired_item = Some(PairedItem::Index(0));
            } else if same_len {
                item.paired_item = Some(PairedItem::Index(i));
            }
        }
    }
}

// ------------------------------------------------------- corpus runner glue

#[cfg(feature = "testing")]
mod corpus {
    use async_trait::async_trait;

    use super::{ExecuteInput, Executor};
    use crate::testing::{Execution, FlowRunner, NodeRun, RunData, RunDataMain, RunInput};

    #[async_trait]
    impl FlowRunner for Executor {
        async fn run(&self, input: RunInput<'_>) -> Execution {
            let req = ExecuteInput::new(input.trigger_node, input.trigger_items.to_vec());
            match self.execute(input.workflow, &req).await {
                Err(e) => Execution::Failed(e.to_string()),
                Ok(res) => {
                    let data: RunData = res
                        .run_data
                        .into_iter()
                        .map(|(name, runs)| {
                            let runs = runs
                                .into_iter()
                                .map(|r| NodeRun {
                                    data: RunDataMain { main: r.data.main },
                                    error: r.error.and_then(|e| serde_json::to_value(e).ok()),
                                })
                                .collect();
                            (name, runs)
                        })
                        .collect();
                    Execution::Done(data)
                }
            }
        }
    }
}
