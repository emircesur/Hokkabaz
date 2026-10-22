//! Polling triggers: n8n `poll()` nodes on top of hk-flow's trigger runtime.
//!
//! hk-flow's `LocalTriggerHost` arms schedule/webhook/app-event/file-watch triggers only,
//! so [`PollingHost`] wraps a `TriggerHost`: workflows with polling nodes get one task per
//! node here, everything else is delegated. Each cycle calls [`PollingTrigger::poll`] with
//! the cursor persisted last time, drops items already seen, persists the new state and
//! only then emits the fresh items as a `TriggerEvent` (at-most-once: a crash between
//! save and emit loses a batch rather than replaying it).
//!
//! State per `(workflow, node)` is `{ cursor, seen: [ids] }`, kept in the workflow's static
//! data (`node:<name>` scope) through [`StaticDataPollState`], like n8n's
//! `getWorkflowStaticData('node')`.
//!
//! The first poll of a node is a baseline: it records the cursor and ids but emits nothing,
//! unless the trigger opts in with [`PollingTrigger::emit_on_first_poll`]. This is n8n's
//! activation behaviour and avoids replaying history when a workflow is switched on.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use tokio::task::JoinHandle;

use hk_flow::description::NodeTypeDescription;
use hk_flow::run_data::{StaticData, node_scope};
use hk_flow::triggers::{
    Activation, TriggerEvent, TriggerHost, TriggerKind, TriggerMode, TriggerSink,
};
use hk_flow::{
    CredentialResolver, FlowError, FlowNode, Item, Node, NodeContext, NodeOutput, NodeRegistry,
    Workflow,
};

/// Ids remembered for dedupe per node. Bounds the persisted state.
pub const DEDUPE_WINDOW: usize = 1000;

// ------------------------------------------------------------------- source

/// One record a source returned. `id` is the dedupe key (stable across polls).
#[derive(Debug, Clone, PartialEq)]
pub struct PolledItem {
    pub id: String,
    pub json: Value,
}

impl PolledItem {
    pub fn new(id: impl Into<String>, json: Value) -> Self {
        Self { id: id.into(), json }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PollBatch {
    pub items: Vec<PolledItem>,
    /// New cursor to hand to the next poll; `None` keeps the previous one.
    pub cursor: Option<Value>,
}

/// What a poll sees: the node (parameters with expression support, credentials).
#[derive(Debug, Clone)]
pub struct PollContext {
    ctx: NodeContext,
}

impl PollContext {
    /// Context for a trigger node outside an execution: one empty input item so parameter
    /// expressions evaluate.
    pub fn for_node(
        node: &Node,
        workflow_id: Option<&str>,
        credentials: Option<Arc<dyn CredentialResolver>>,
    ) -> Self {
        let mut ctx = NodeContext::new(node.clone(), vec![vec![Item::default()]], 0);
        ctx.workflow_id = workflow_id.map(str::to_owned);
        ctx.hooks.credentials = credentials;
        Self { ctx }
    }

    pub fn from_node_context(ctx: &NodeContext) -> Self {
        Self { ctx: ctx.clone() }
    }

    pub fn node(&self) -> &Node {
        &self.ctx.node
    }

    pub fn param(&self, path: &str) -> Result<Option<Value>, FlowError> {
        self.ctx.eval_param_at(path, 0)
    }

    pub fn param_str(&self, path: &str, default: &str) -> Result<String, FlowError> {
        Ok(match self.param(path)? {
            Some(Value::String(s)) => s,
            Some(Value::Null) | None => default.to_owned(),
            Some(v) => crate::template::display(&v),
        })
    }

    /// Decrypted credential fields of the node's `credential_type` reference. Use and drop;
    /// never put them in items or logs.
    pub async fn credential(&self, credential_type: &str) -> Result<Map<String, Value>, FlowError> {
        let reference = self
            .ctx
            .node
            .credentials
            .as_ref()
            .and_then(|c| c.get(credential_type))
            .ok_or_else(|| FlowError::Node {
                node: self.ctx.node.name.clone(),
                message: format!("No credentials of type `{credential_type}` selected"),
            })?;
        let resolver = self.ctx.hooks.credentials.as_ref().ok_or_else(|| FlowError::Node {
            node: self.ctx.node.name.clone(),
            message: "Credentials are not available in this runtime".into(),
        })?;
        resolver.resolve(credential_type, reference).await
    }
}

#[async_trait]
pub trait PollingTrigger: Send + Sync + fmt::Debug {
    /// Trigger description (`NodeBuilder::polling_trigger`).
    fn description(&self) -> &NodeTypeDescription;

    /// Emit the first poll's items instead of treating it as a baseline.
    fn emit_on_first_poll(&self) -> bool {
        false
    }

    /// Fetch records. `cursor` is what the previous batch returned (`None` on the first
    /// poll). Returning already-seen records is fine: they are deduped by id.
    async fn poll(&self, cx: &PollContext, cursor: Option<&Value>) -> Result<PollBatch, FlowError>;
}

// -------------------------------------------------------------------- state

/// Persisted poll state per `(workflow, node)`.
#[async_trait]
pub trait PollStateStore: Send + Sync + fmt::Debug {
    async fn load(&self, workflow_id: &str, node: &str) -> Result<Option<Value>, FlowError>;
    async fn save(&self, workflow_id: &str, node: &str, state: Value) -> Result<(), FlowError>;
}

/// Volatile state (tests, embedders without a store). A restart re-baselines.
#[derive(Debug, Default)]
pub struct MemoryPollState(Mutex<HashMap<(String, String), Value>>);

#[async_trait]
impl PollStateStore for MemoryPollState {
    async fn load(&self, workflow_id: &str, node: &str) -> Result<Option<Value>, FlowError> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(workflow_id.to_owned(), node.to_owned()))
            .cloned())
    }

    async fn save(&self, workflow_id: &str, node: &str, state: Value) -> Result<(), FlowError> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((workflow_id.to_owned(), node.to_owned()), state);
        Ok(())
    }
}

/// Where a workflow's whole `staticData` value lives (the daemon's workflow row).
#[async_trait]
pub trait StaticDataStore: Send + Sync + fmt::Debug {
    async fn load_static(&self, workflow_id: &str) -> Result<Option<Value>, FlowError>;
    async fn save_static(&self, workflow_id: &str, data: Value) -> Result<(), FlowError>;
}

/// [`PollStateStore`] over workflow static data, scope `node:<name>`. Read-modify-write is
/// serialized per adapter; an execution that finishes later and writes back a stale
/// snapshot of the whole static data can still clobber it (the same race n8n has).
#[derive(Debug)]
pub struct StaticDataPollState<S> {
    store: S,
    lock: tokio::sync::Mutex<()>,
}

impl<S: StaticDataStore> StaticDataPollState<S> {
    pub fn new(store: S) -> Self {
        Self { store, lock: tokio::sync::Mutex::new(()) }
    }
}

#[async_trait]
impl<S: StaticDataStore> PollStateStore for StaticDataPollState<S> {
    async fn load(&self, workflow_id: &str, node: &str) -> Result<Option<Value>, FlowError> {
        let raw = self.store.load_static(workflow_id).await?;
        let sd = StaticData::from_value(raw.as_ref());
        Ok(Some(sd.get(&node_scope(node))).filter(|v| v.as_object().is_some_and(|m| !m.is_empty())))
    }

    async fn save(&self, workflow_id: &str, node: &str, state: Value) -> Result<(), FlowError> {
        let _g = self.lock.lock().await;
        let raw = self.store.load_static(workflow_id).await?;
        let sd = StaticData::from_value(raw.as_ref());
        sd.set(&node_scope(node), state);
        self.store.save_static(workflow_id, sd.snapshot()).await
    }
}

// -------------------------------------------------------------------- cycle

/// Result of one poll cycle.
#[derive(Debug, Clone, PartialEq)]
pub struct PollOutcome {
    /// Fresh items to emit (empty on the baseline poll).
    pub items: Vec<Item>,
    pub baseline: bool,
}

/// One cycle: load state, poll, dedupe, persist. Does not emit.
pub async fn run_poll(
    trigger: &dyn PollingTrigger,
    store: &dyn PollStateStore,
    cx: &PollContext,
    workflow_id: &str,
) -> Result<PollOutcome, FlowError> {
    let node = cx.node().name.clone();
    let prev = store.load(workflow_id, &node).await?;
    let baseline = prev.is_none();
    let cursor = prev.as_ref().and_then(|s| s.get("cursor")).filter(|c| !c.is_null());
    let mut seen: VecDeque<String> = prev
        .as_ref()
        .and_then(|s| s.get("seen"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();

    let batch = trigger.poll(cx, cursor).await?;

    let mut known: HashSet<&str> = seen.iter().map(String::as_str).collect();
    let mut fresh = Vec::new();
    let mut fresh_ids = Vec::new();
    for it in &batch.items {
        if known.insert(it.id.as_str()) {
            fresh_ids.push(it.id.clone());
            fresh.push(Item::from_json(it.json.clone()));
        }
    }
    seen.extend(fresh_ids);
    while seen.len() > DEDUPE_WINDOW {
        seen.pop_front();
    }
    let next_cursor = batch.cursor.or_else(|| cursor.cloned()).unwrap_or(Value::Null);

    store
        .save(
            workflow_id,
            &node,
            json!({ "cursor": next_cursor, "seen": seen.iter().collect::<Vec<_>>() }),
        )
        .await?;

    let silent = baseline && !trigger.emit_on_first_poll();
    Ok(PollOutcome { items: if silent { Vec::new() } else { fresh }, baseline })
}

/// Poll interval from the node's `pollTimes` parameter (shortest entry); one minute when
/// unset. Read statically: no expressions.
pub fn poll_interval(node: &Node) -> Duration {
    let items = node
        .parameters
        .get("pollTimes")
        .and_then(|p| p.get("item"))
        .and_then(Value::as_array);
    let one = |it: &Value| -> Option<Duration> {
        match it.get("mode").and_then(Value::as_str)? {
            "everyMinute" => Some(Duration::from_secs(60)),
            "everyHour" => Some(Duration::from_secs(3600)),
            "everyX" => {
                let n = it.get("value").and_then(Value::as_f64).filter(|n| *n > 0.0)?;
                let unit = match it.get("unit").and_then(Value::as_str).unwrap_or("minutes") {
                    "seconds" => 1.0,
                    "hours" => 3600.0,
                    _ => 60.0,
                };
                Some(Duration::from_secs_f64(n * unit))
            }
            _ => None,
        }
    };
    items
        .and_then(|a| a.iter().filter_map(one).min())
        .unwrap_or(Duration::from_secs(60))
}

// ----------------------------------------------------------------- registry

/// Polling trigger implementations by node type.
#[derive(Debug, Default, Clone)]
pub struct PollRegistry(HashMap<String, Arc<dyn PollingTrigger>>);

impl PollRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the trigger and its node (manual "fetch test event" run) in `nodes`.
    pub fn register(
        &mut self,
        nodes: &mut NodeRegistry,
        trigger: Arc<dyn PollingTrigger>,
    ) -> Result<(), FlowError> {
        let ty = trigger.description().name.clone();
        nodes.register(Arc::new(PollingNode(Arc::clone(&trigger))))?;
        self.0.insert(ty, trigger);
        Ok(())
    }

    pub fn get(&self, node_type: &str) -> Option<&Arc<dyn PollingTrigger>> {
        self.0.get(node_type)
    }
}

/// `FlowNode` face of a polling trigger. When the host fired it, the executor passes the
/// polled items as input and they flow on. Run by hand (no input, canvas "test"), it polls
/// once without touching persisted state.
#[derive(Debug)]
pub struct PollingNode(pub Arc<dyn PollingTrigger>);

#[async_trait]
impl FlowNode for PollingNode {
    fn description(&self) -> &NodeTypeDescription {
        self.0.description()
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let given = ctx.input.first().cloned().unwrap_or_default();
        if !given.is_empty() {
            return Ok(vec![given]);
        }
        let cx = PollContext::from_node_context(ctx);
        let batch = self.0.poll(&cx, None).await?;
        let items: Vec<Item> = batch.items.into_iter().map(|i| Item::from_json(i.json)).collect();
        Ok(vec![if items.is_empty() { vec![Item::default()] } else { items }])
    }
}

// --------------------------------------------------------------------- host

struct Shared {
    triggers: PollRegistry,
    state: Arc<dyn PollStateStore>,
    sink: Arc<dyn TriggerSink>,
    credentials: Option<Arc<dyn CredentialResolver>>,
    min_interval: Duration,
}

/// `TriggerHost` that also arms polling trigger nodes. Wrap the daemon's
/// `LocalTriggerHost` with it; both share one sink.
pub struct PollingHost {
    inner: Arc<dyn TriggerHost>,
    shared: Arc<Shared>,
    tasks: Mutex<HashMap<String, Vec<JoinHandle<()>>>>,
}

impl fmt::Debug for PollingHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PollingHost")
            .field("polling", &self.tasks.lock().unwrap_or_else(PoisonError::into_inner).keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl PollingHost {
    pub fn new(
        inner: Arc<dyn TriggerHost>,
        triggers: PollRegistry,
        state: Arc<dyn PollStateStore>,
        sink: Arc<dyn TriggerSink>,
        credentials: Option<Arc<dyn CredentialResolver>>,
    ) -> Self {
        Self {
            inner,
            shared: Arc::new(Shared {
                triggers,
                state,
                sink,
                credentials,
                min_interval: Duration::from_secs(10),
            }),
            tasks: Mutex::new(HashMap::new()),
        }
    }

    /// Floor for node-configured intervals (default 10 s) so a typo cannot hammer an API.
    pub fn with_min_interval(mut self, d: Duration) -> Self {
        if let Some(s) = Arc::get_mut(&mut self.shared) {
            s.min_interval = d;
        }
        self
    }

    /// Number of polling nodes armed for `workflow_id`.
    pub fn polling_count(&self, workflow_id: &str) -> usize {
        self.tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workflow_id)
            .map_or(0, Vec::len)
    }

    /// Run one cycle of `node` now and emit what it found. Returns the emitted count.
    /// The timer tasks call this; tests call it directly.
    pub async fn poll_node(&self, workflow_id: &str, node: &Node) -> Result<usize, FlowError> {
        poll_and_emit(&self.shared, workflow_id, node).await
    }

    fn stop(&self, workflow_id: &str) {
        if let Some(ts) = self.tasks.lock().unwrap_or_else(PoisonError::into_inner).remove(workflow_id) {
            for t in ts {
                t.abort();
            }
        }
    }
}

async fn poll_and_emit(sh: &Shared, workflow_id: &str, node: &Node) -> Result<usize, FlowError> {
    let trigger = sh.triggers.get(&node.node_type).ok_or_else(|| FlowError::Node {
        node: node.name.clone(),
        message: "not a polling trigger".into(),
    })?;
    let cx = PollContext::for_node(node, Some(workflow_id), sh.credentials.clone());
    let out = run_poll(trigger.as_ref(), sh.state.as_ref(), &cx, workflow_id).await?;
    let n = out.items.len();
    if n > 0 {
        sh.sink.emit(TriggerEvent {
            workflow_id: workflow_id.to_owned(),
            node_name: Some(node.name.clone()),
            items: out.items,
            mode: TriggerMode::Trigger,
        });
    }
    Ok(n)
}

#[async_trait]
impl TriggerHost for PollingHost {
    async fn activate(&self, workflow: &Workflow) -> Result<Activation, FlowError> {
        let polling: Vec<&Node> = workflow
            .nodes
            .iter()
            .filter(|n| !n.is_disabled() && self.shared.triggers.get(&n.node_type).is_some())
            .collect();
        if polling.is_empty() {
            return self.inner.activate(workflow).await;
        }
        let id = workflow
            .id
            .clone()
            .ok_or_else(|| FlowError::Invalid("workflow has no id".into()))?;
        let delegate = workflow.nodes.iter().any(|n| {
            !n.is_disabled()
                && TriggerKind::of_type(&n.node_type).is_some_and(|k| k != TriggerKind::Manual)
        });
        // the inner host refuses a workflow without a trigger it knows
        let activation = if delegate {
            self.inner.activate(workflow).await?
        } else {
            self.inner.deactivate(&id).await?;
            Activation::default()
        };

        self.stop(&id);
        let mut handles = Vec::new();
        for node in polling {
            let (sh, wf_id, node) = (Arc::clone(&self.shared), id.clone(), node.clone());
            let every = poll_interval(&node).max(sh.min_interval);
            handles.push(tokio::spawn(async move {
                // first tick is immediate: it is the baseline poll
                let mut iv = tokio::time::interval(every);
                iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    iv.tick().await;
                    if let Err(e) = poll_and_emit(&sh, &wf_id, &node).await {
                        tracing::warn!(workflow = %wf_id, node = %node.name, error = %e, "poll failed");
                    }
                }
            }));
        }
        self.tasks.lock().unwrap_or_else(PoisonError::into_inner).insert(id, handles);
        Ok(activation)
    }

    async fn deactivate(&self, workflow_id: &str) -> Result<(), FlowError> {
        self.stop(workflow_id);
        self.inner.deactivate(workflow_id).await
    }

    fn is_active(&self, workflow_id: &str) -> bool {
        self.polling_count(workflow_id) > 0 || self.inner.is_active(workflow_id)
    }
}

impl Drop for PollingHost {
    fn drop(&mut self) {
        for ts in self.tasks.get_mut().unwrap_or_else(PoisonError::into_inner).values() {
            for t in ts {
                t.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use hk_flow::triggers::{ChannelSink, LocalTriggerHost, WebhookRegistry};

    use super::*;
    use crate::describe::{NodeBuilder, poll_times_prop};

    /// Source over an in-memory "remote" list; returns everything with id > cursor.
    #[derive(Debug)]
    struct Fake {
        desc: NodeTypeDescription,
        remote: Mutex<Vec<(u64, &'static str)>>,
        polls: AtomicUsize,
        first: bool,
    }

    impl Fake {
        fn new(first: bool) -> Arc<Self> {
            Arc::new(Self {
                desc: NodeBuilder::new("hokkabaz.fakePoll", "Fake Poll")
                    .polling_trigger()
                    .property(poll_times_prop())
                    .build(),
                remote: Mutex::new(vec![(1, "a"), (2, "b")]),
                polls: AtomicUsize::new(0),
                first,
            })
        }
        fn push(&self, id: u64, t: &'static str) {
            self.remote.lock().unwrap().push((id, t));
        }
    }

    #[async_trait]
    impl PollingTrigger for Fake {
        fn description(&self) -> &NodeTypeDescription {
            &self.desc
        }
        fn emit_on_first_poll(&self) -> bool {
            self.first
        }
        async fn poll(&self, _cx: &PollContext, cursor: Option<&Value>) -> Result<PollBatch, FlowError> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            let after = cursor.and_then(Value::as_u64).unwrap_or(0);
            let all = self.remote.lock().unwrap().clone();
            // deliberately also returns the already-seen last record: dedupe must drop it
            let items: Vec<_> = all
                .iter()
                .filter(|(id, _)| *id + 1 >= after.max(1))
                .map(|(id, t)| PolledItem::new(id.to_string(), json!({"id": id, "text": t})))
                .collect();
            Ok(PollBatch { cursor: all.last().map(|(id, _)| json!(id)), items })
        }
    }

    fn node() -> Node {
        let mut n = Node::new("Poll", "hokkabaz.fakePoll", serde_json::Number::from(1), [0.into(), 0.into()]);
        n.parameters = match json!({"pollTimes": {"item": [{"mode": "everyX", "value": 30, "unit": "seconds"}]}}) {
            Value::Object(m) => m,
            _ => unreachable!(),
        };
        n
    }

    fn ids(items: &[Item]) -> Vec<u64> {
        items.iter().map(|i| i.json["id"].as_u64().unwrap()).collect()
    }

    #[tokio::test]
    async fn baseline_then_only_new_items() {
        let src = Fake::new(false);
        let store = MemoryPollState::default();
        let cx = PollContext::for_node(&node(), Some("wf"), None);

        let o = run_poll(src.as_ref(), &store, &cx, "wf").await.unwrap();
        assert!(o.baseline && o.items.is_empty());
        // nothing new: the overlap record is deduped
        let o = run_poll(src.as_ref(), &store, &cx, "wf").await.unwrap();
        assert!(!o.baseline && o.items.is_empty());

        src.push(3, "c");
        src.push(4, "d");
        let o = run_poll(src.as_ref(), &store, &cx, "wf").await.unwrap();
        assert_eq!(ids(&o.items), vec![3, 4]);
        let o = run_poll(src.as_ref(), &store, &cx, "wf").await.unwrap();
        assert!(o.items.is_empty());

        let st = store.load("wf", "Poll").await.unwrap().unwrap();
        assert_eq!(st["cursor"], 4);
        assert_eq!(st["seen"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn first_poll_can_emit_and_window_is_bounded() {
        let src = Fake::new(true);
        let store = MemoryPollState::default();
        let cx = PollContext::for_node(&node(), Some("wf"), None);
        let o = run_poll(src.as_ref(), &store, &cx, "wf").await.unwrap();
        assert_eq!(ids(&o.items), vec![1, 2]);

        for i in 3..(DEDUPE_WINDOW as u64 + 20) {
            src.push(i, "x");
        }
        run_poll(src.as_ref(), &store, &cx, "wf").await.unwrap();
        let st = store.load("wf", "Poll").await.unwrap().unwrap();
        assert_eq!(st["seen"].as_array().unwrap().len(), DEDUPE_WINDOW);
    }

    #[test]
    fn intervals() {
        assert_eq!(poll_interval(&node()), Duration::from_secs(30));
        let mut n = node();
        n.parameters.clear();
        assert_eq!(poll_interval(&n), Duration::from_secs(60));
        n.parameters = match json!({"pollTimes": {"item": [{"mode": "everyHour"}, {"mode": "everyX", "value": 2, "unit": "minutes"}]}}) {
            Value::Object(m) => m,
            _ => unreachable!(),
        };
        assert_eq!(poll_interval(&n), Duration::from_secs(120));
    }

    #[derive(Debug, Default)]
    struct MemStatic(Mutex<HashMap<String, Value>>);

    #[async_trait]
    impl StaticDataStore for Arc<MemStatic> {
        async fn load_static(&self, id: &str) -> Result<Option<Value>, FlowError> {
            Ok(self.0.lock().unwrap().get(id).cloned())
        }
        async fn save_static(&self, id: &str, data: Value) -> Result<(), FlowError> {
            self.0.lock().unwrap().insert(id.to_owned(), data);
            Ok(())
        }
    }

    #[tokio::test]
    async fn state_lives_in_workflow_static_data() {
        let mem = Arc::new(MemStatic::default());
        // another node's scope must survive our writes
        mem.0.lock().unwrap().insert("wf".into(), json!({"global": {"k": 1}}));
        let store = StaticDataPollState::new(Arc::clone(&mem));
        let src = Fake::new(false);
        let cx = PollContext::for_node(&node(), Some("wf"), None);
        run_poll(src.as_ref(), &store, &cx, "wf").await.unwrap();
        let raw = mem.0.lock().unwrap().get("wf").cloned().unwrap();
        assert_eq!(raw["global"]["k"], 1);
        assert_eq!(raw["node:Poll"]["cursor"], 2);
        // a fresh adapter (restart) continues from the persisted cursor
        let store2 = StaticDataPollState::new(Arc::clone(&mem));
        src.push(3, "c");
        let o = run_poll(src.as_ref(), &store2, &cx, "wf").await.unwrap();
        assert_eq!(ids(&o.items), vec![3]);
    }

    fn workflow(with_schedule: bool) -> Workflow {
        let mut nodes = vec![json!({"name": "Poll", "type": "hokkabaz.fakePoll", "typeVersion": 1, "position": [0, 0],
            "parameters": {"pollTimes": {"item": [{"mode": "everyX", "value": 1, "unit": "seconds"}]}}})];
        if with_schedule {
            nodes.push(json!({"name": "Sched", "type": "n8n-nodes-base.scheduleTrigger", "typeVersion": 1.2, "position": [0, 100],
                "parameters": {"rule": {"interval": [{"field": "hours", "hoursInterval": 1}]}}}));
        }
        Workflow::from_json_value(json!({"id": "wf1", "name": "t", "nodes": nodes, "connections": {}})).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn host_arms_polls_and_emits_through_sink() {
        let src = Fake::new(false);
        let mut nodes = NodeRegistry::new();
        let mut polls = PollRegistry::new();
        polls.register(&mut nodes, src.clone()).unwrap();
        assert!(nodes.latest("hokkabaz.fakePoll").is_some());

        let (sink, mut rx) = ChannelSink::channel();
        let sink: Arc<dyn TriggerSink> = Arc::new(sink);
        let inner = Arc::new(LocalTriggerHost::new(Arc::clone(&sink), Arc::new(WebhookRegistry::new())));
        let host = PollingHost::new(inner.clone(), polls, Arc::new(MemoryPollState::default()), sink, None)
            .with_min_interval(Duration::from_secs(1));

        // poll-only workflow: inner host would refuse it, we still activate
        host.activate(&workflow(false)).await.unwrap();
        assert!(host.is_active("wf1") && !inner.is_active("wf1"));
        assert_eq!(host.polling_count("wf1"), 1);

        tokio::time::sleep(Duration::from_millis(100)).await; // baseline tick
        assert_eq!(src.polls.load(Ordering::SeqCst), 1);
        assert!(rx.try_recv().is_err());

        src.push(3, "c");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let ev = rx.try_recv().unwrap();
        assert_eq!(ev.workflow_id, "wf1");
        assert_eq!(ev.node_name.as_deref(), Some("Poll"));
        assert_eq!(ids(&ev.items), vec![3]);

        host.deactivate("wf1").await.unwrap();
        assert!(!host.is_active("wf1"));
        let n = src.polls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(src.polls.load(Ordering::SeqCst), n);

        // mixed workflow: schedule part delegated to the inner host
        host.activate(&workflow(true)).await.unwrap();
        assert!(inner.is_active("wf1"));
        host.deactivate("wf1").await.unwrap();
        assert!(!inner.is_active("wf1"));
    }

    #[tokio::test]
    async fn manual_run_polls_without_state() {
        let src = Fake::new(false);
        let n = PollingNode(src);
        let ctx = NodeContext::new(node(), vec![vec![]], 0);
        let out = n.execute(&ctx).await.unwrap();
        assert_eq!(ids(&out[0]), vec![1, 2]);
        // host-fired: input passes through
        let ctx = NodeContext::new(node(), vec![vec![Item::from_json(json!({"id": 9}))]], 0);
        assert_eq!(ids(&n.execute(&ctx).await.unwrap()[0]), vec![9]);
    }
}
