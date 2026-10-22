//! Flow test runs: the in-process executor behind `run_workflow` ([`LocalFlows`]) and the
//! event/size helpers [`crate::remote::RemoteCore`] shares.
//!
//! A run is an `hk_flow::Executor` task whose `RunStore` checkpoints (after every node) are
//! diffed into [`FlowRunEventDto`]s. Events are buffered per run, so a subscriber that attaches
//! after `run_workflow` returned still replays from the start. Finished runs are written to
//! `<data_dir>/flow_runs/<id>.json` (+ a small `<id>.meta.json` the list reads).
//!
//! Everything that leaves the engine is bounded and scrubbed: binary payloads dropped, item
//! lists cut, credential-bearing headers blanked (execution data never holds secrets).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use hk_flow::triggers::{TriggerKind, find_trigger};
use hk_flow::{
    ExecuteInput, ExecutionRecord, Executor, ExecutorConfig, FlowError, Item, NodeRegistry,
    RunStatus, RunStore, Workflow,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::watch;

use crate::dto::{
    FlowRunDetailDto, FlowRunEventDto, FlowRunEventKind as K, FlowRunModeDto, FlowRunSummaryDto,
};
use crate::error::{HkError, HkErrorKind};

/// Per-output caps for live events (the UI renders them in a panel).
const EVENT_MAX_ITEMS: usize = 50;
const EVENT_MAX_BYTES: usize = 128 * 1024;
/// Per-output caps for persisted / fetched run detail.
pub(crate) const DETAIL_MAX_ITEMS: usize = 500;
pub(crate) const DETAIL_MAX_BYTES: usize = 512 * 1024;
/// Trigger items a caller may inject.
pub(crate) const MAX_INPUT_ITEMS: usize = 200;
/// Runs kept on disk.
const KEEP_RUNS: usize = 100;
/// Runs kept in memory for late subscribers.
const KEEP_LIVE: usize = 16;
/// An interactive run that is still going after this is abandoned.
const RUN_LIMIT: Duration = Duration::from_secs(600);

const SENSITIVE_HEADERS: [&str; 6] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-n8n-api-key",
    "x-api-key",
];

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ------------------------------------------------------------------------ bounding / scrubbing

fn scrub_headers(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, child) in m.iter_mut() {
                if k.eq_ignore_ascii_case("headers")
                    && let Value::Object(h) = child
                {
                    for (name, val) in h.iter_mut() {
                        if SENSITIVE_HEADERS
                            .iter()
                            .any(|s| name.eq_ignore_ascii_case(s))
                        {
                            *val = Value::String("[redacted]".into());
                        }
                    }
                }
                scrub_headers(child);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(scrub_headers),
        _ => {}
    }
}

/// Drop inline binary payloads (base64 can be megabytes); keep name / mime type.
fn strip_binary(item: &mut Value) {
    let Some(bin) = item.get_mut("binary").and_then(Value::as_object_mut) else {
        return;
    };
    for b in bin.values_mut() {
        if let Some(o) = b.as_object_mut()
            && o.remove("data").is_some()
        {
            o.insert("dataOmitted".into(), Value::Bool(true));
        }
    }
}

fn json_len(v: &Value) -> usize {
    serde_json::to_string(v).map_or(0, |s| s.len())
}

/// Bound `main` (`[[item, ..] | null, ..]`) in place. Returns the true item count per output and
/// whether anything was cut.
pub(crate) fn shrink_main(
    main: &mut Value,
    max_items: usize,
    max_bytes: usize,
) -> (Vec<u32>, bool) {
    let mut counts = Vec::new();
    let mut cut = false;
    let Some(slots) = main.as_array_mut() else {
        return (counts, cut);
    };
    for slot in slots {
        scrub_headers(slot);
        let Some(items) = slot.as_array_mut() else {
            counts.push(0);
            continue;
        };
        counts.push(u32::try_from(items.len()).unwrap_or(u32::MAX));
        items.iter_mut().for_each(strip_binary);
        let mut keep = items.len().min(max_items);
        items.truncate(keep);
        while keep > 1 && json_len(slot) > max_bytes {
            keep /= 2;
            if let Some(items) = slot.as_array_mut() {
                items.truncate(keep);
            }
        }
        if let Some(items) = slot.as_array_mut() {
            if keep == 1 && json_len(&items[0]) > max_bytes {
                items[0] = json!({ "json": { "truncated": format!("item larger than {max_bytes} bytes") } });
            }
            cut |= u32::try_from(items.len()).unwrap_or(u32::MAX) < *counts.last().unwrap_or(&0);
        }
    }
    (counts, cut)
}

/// Bound every run of every node of an n8n `runData` object; adds `itemCounts` (true counts)
/// and `truncated` to each run.
pub(crate) fn shrink_run_data(run_data: &mut Value, max_items: usize, max_bytes: usize) {
    let Some(nodes) = run_data.as_object_mut() else {
        return;
    };
    for runs in nodes.values_mut() {
        let Some(runs) = runs.as_array_mut() else {
            continue;
        };
        for run in runs {
            let Some(run) = run.as_object_mut() else {
                continue;
            };
            let mut cut = false;
            let mut counts = Vec::new();
            if let Some(main) = run.get_mut("data").and_then(|d| d.get_mut("main")) {
                (counts, cut) = shrink_main(main, max_items, max_bytes);
            }
            run.insert("itemCounts".into(), json!(counts));
            if cut {
                run.insert("truncated".into(), Value::Bool(true));
            }
            if let Some(src) = run.get_mut("source") {
                scrub_headers(src);
            }
        }
    }
}

// --------------------------------------------------------------------------------- events

fn u64_at(v: &Value, key: &str) -> i64 {
    v.get(key)
        .and_then(Value::as_u64)
        .map_or(0, |n| i64::try_from(n).unwrap_or(i64::MAX))
}

/// `NodeStarted` + `NodeFinished` for one recorded node run (n8n run JSON).
fn node_events(run_id: &str, node: &str, idx: usize, run: &Value) -> [FlowRunEventDto; 2] {
    let started = u64_at(run, "startTime");
    let mut main = run
        .pointer("/data/main")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let (counts, cut) = shrink_main(&mut main, EVENT_MAX_ITEMS, EVENT_MAX_BYTES);
    let mut s = FlowRunEventDto::new(run_id, K::NodeStarted);
    s.node = Some(node.to_owned());
    s.run_index = u32::try_from(idx).unwrap_or(u32::MAX);
    s.started_ms = started;
    let mut f = s.clone();
    f.kind = K::NodeFinished;
    f.status = Some(
        run.get("executionStatus")
            .and_then(Value::as_str)
            .unwrap_or("success")
            .to_owned(),
    );
    f.item_count = counts.iter().sum();
    f.duration_ms = u64_at(run, "executionTime");
    f.output_json = serde_json::to_string(&main).ok();
    f.truncated = cut;
    f.error = run
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_owned);
    [s, f]
}

fn terminal(
    run_id: &str,
    status: &str,
    error: Option<String>,
    node: Option<String>,
    started: i64,
    ended: i64,
) -> FlowRunEventDto {
    let mut e = FlowRunEventDto::new(
        run_id,
        if status == "error" {
            K::RunError
        } else {
            K::RunFinished
        },
    );
    e.status = Some(status.to_owned());
    e.error = error;
    e.node = node;
    e.started_ms = started;
    e.duration_ms = (ended - started).max(0);
    e
}

/// The whole event sequence of a finished run, rebuilt from its stored data (remote runs, local
/// runs evicted from memory).
pub(crate) fn events_from_detail(d: &FlowRunDetailDto) -> Vec<FlowRunEventDto> {
    let s = &d.summary;
    let mut start = FlowRunEventDto::new(&s.id, K::RunStarted);
    start.started_ms = s.started_ms;
    let mut out = vec![start];
    let data: Value = serde_json::from_str(&d.run_data_json).unwrap_or(Value::Null);
    let mut runs: Vec<(i64, &str, usize, &Value)> = Vec::new();
    if let Some(nodes) = data.as_object() {
        for (name, rs) in nodes {
            for (i, r) in rs.as_array().into_iter().flatten().enumerate() {
                runs.push((u64_at(r, "executionIndex"), name, i, r));
            }
        }
    }
    runs.sort_by_key(|(order, ..)| *order);
    for (_, name, i, r) in runs {
        out.extend(node_events(&s.id, name, i, r));
    }
    let ended = s.finished_ms.unwrap_or(s.started_ms);
    out.push(terminal(
        &s.id,
        &s.status,
        s.error.clone(),
        None,
        s.started_ms,
        ended,
    ));
    out
}

// ----------------------------------------------------------------------------------- live hub

/// Buffered, followable event log of one run.
#[derive(Debug)]
pub(crate) struct Live {
    events: Mutex<Vec<FlowRunEventDto>>,
    version: watch::Sender<usize>,
    done: Mutex<bool>,
}

impl Live {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            version: watch::channel(0).0,
            done: Mutex::new(false),
        })
    }

    fn push(&self, ev: FlowRunEventDto) {
        let n = {
            let mut g = lock(&self.events);
            if ev.is_terminal() {
                *lock(&self.done) = true;
            }
            g.push(ev);
            g.len()
        };
        self.version.send_replace(n);
    }

    fn finished(&self) -> bool {
        *lock(&self.done)
    }

    /// Replays the buffer, then follows live until the terminal event.
    fn follow(self: Arc<Self>) -> BoxStream<'static, FlowRunEventDto> {
        let rx = self.version.subscribe();
        stream::unfold(
            (self, rx, 0usize, false),
            |(live, mut rx, mut idx, done)| async move {
                if done {
                    return None;
                }
                loop {
                    let ev = lock(&live.events).get(idx).cloned();
                    if let Some(ev) = ev {
                        idx += 1;
                        let done = ev.is_terminal();
                        return Some((ev, (live, rx, idx, done)));
                    }
                    if rx.changed().await.is_err() {
                        return None;
                    }
                }
            },
        )
        .boxed()
    }
}

/// `RunStore` that only turns checkpoints into events.
#[derive(Debug)]
struct LiveStore {
    run_id: String,
    live: Arc<Live>,
    /// Runs already announced per node.
    seen: Mutex<HashMap<String, usize>>,
}

#[async_trait]
impl RunStore for LiveStore {
    async fn save(&self, record: &ExecutionRecord) -> Result<(), FlowError> {
        let mut fresh: Vec<(usize, String, usize, Value)> = Vec::new();
        {
            let mut seen = lock(&self.seen);
            for (node, runs) in &record.run_data {
                let from = seen.get(node).copied().unwrap_or(0);
                for (i, r) in runs.iter().enumerate().skip(from) {
                    if let Ok(v) = serde_json::to_value(r) {
                        fresh.push((r.execution_index, node.clone(), i, v));
                    }
                }
                seen.insert(node.clone(), runs.len());
            }
        }
        fresh.sort_by_key(|(order, ..)| *order);
        for (_, node, i, v) in fresh {
            for ev in node_events(&self.run_id, &node, i, &v) {
                self.live.push(ev);
            }
        }
        Ok(())
    }

    async fn load(&self, _id: &str) -> Result<Option<ExecutionRecord>, FlowError> {
        Ok(None)
    }

    async fn list(&self, _workflow_id: Option<&str>) -> Result<Vec<ExecutionRecord>, FlowError> {
        Ok(Vec::new())
    }

    async fn delete(&self, _id: &str) -> Result<bool, FlowError> {
        Ok(false)
    }
}

// -------------------------------------------------------------------------- persisted shapes

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    id: String,
    workflow_id: Option<String>,
    workflow_name: String,
    mode: String,
    status: String,
    started_ms: i64,
    finished_ms: Option<i64>,
    error: Option<String>,
    node_count: u32,
}

impl Meta {
    fn dto(&self) -> FlowRunSummaryDto {
        FlowRunSummaryDto {
            id: self.id.clone(),
            workflow_id: self.workflow_id.clone(),
            workflow_name: self.workflow_name.clone(),
            mode: self.mode.clone(),
            status: self.status.clone(),
            started_ms: self.started_ms,
            finished_ms: self.finished_ms,
            duration_ms: self.finished_ms.map_or(0, |f| (f - self.started_ms).max(0)),
            error: self.error.clone(),
            node_count: self.node_count,
            remote: false,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredRun {
    meta: Meta,
    run_data: Value,
    workflow: Option<Value>,
}

/// Ids become file names: refuse anything that is not one of ours.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn io_err(e: &std::io::Error) -> HkError {
    HkError::internal(format!("flow runs: {e}"))
}

/// Metas on disk, newest first.
fn read_metas(dir: &Path) -> Vec<Meta> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Meta> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".meta.json"))
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter_map(|b| serde_json::from_slice::<Meta>(&b).ok())
        .collect();
    out.sort_by(|a, b| {
        b.started_ms
            .cmp(&a.started_ms)
            .then_with(|| b.id.cmp(&a.id))
    });
    out
}

fn persist(dir: &Path, stored: &StoredRun) -> Result<(), HkError> {
    std::fs::create_dir_all(dir).map_err(|e| io_err(&e))?;
    let id = &stored.meta.id;
    write_atomic(
        &dir.join(format!("{id}.json")),
        &serde_json::to_vec(stored)?,
    )
    .map_err(|e| io_err(&e))?;
    write_atomic(
        &dir.join(format!("{id}.meta.json")),
        &serde_json::to_vec(&stored.meta)?,
    )
    .map_err(|e| io_err(&e))?;
    for old in read_metas(dir).into_iter().skip(KEEP_RUNS) {
        let _ = std::fs::remove_file(dir.join(format!("{}.json", old.id)));
        let _ = std::fs::remove_file(dir.join(format!("{}.meta.json", old.id)));
    }
    Ok(())
}

// ----------------------------------------------------------------------------------- engine

/// Followable runs by id, plus insertion order for eviction.
#[derive(Default)]
struct LiveRuns {
    by_id: HashMap<String, Arc<Live>>,
    order: VecDeque<String>,
}

struct Shared {
    dir: PathBuf,
    credentials: Arc<dyn hk_flow::CredentialResolver>,
    live: Mutex<LiveRuns>,
    seq: AtomicU64,
}

/// In-process test runs; owned by `LocalCore`. Android-safe: no sandbox, no spawn.
pub(crate) struct LocalFlows {
    s: Arc<Shared>,
}

impl std::fmt::Debug for LocalFlows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalFlows")
            .field("dir", &self.s.dir)
            .finish_non_exhaustive()
    }
}

struct Plan {
    wf: Workflow,
    raw: Value,
    start: String,
    items: Vec<Item>,
    name: String,
    workflow_id: Option<String>,
}

/// Parse and check everything before a run id is handed out, so bad input is an immediate error.
fn plan(
    workflow_json: &str,
    trigger_node: Option<&str>,
    input_items_json: Option<&str>,
    pin_data_json: Option<&str>,
    registry: &NodeRegistry,
) -> Result<Plan, HkError> {
    let mut raw: Value = serde_json::from_str(workflow_json)
        .map_err(|e| HkError::invalid(format!("workflow JSON: {e}")))?;
    if let Some(pin) = pin_data_json.filter(|p| !p.trim().is_empty()) {
        let pin: Value = serde_json::from_str(pin)
            .map_err(|e| HkError::invalid(format!("pin data JSON: {e}")))?;
        if !pin.is_object() {
            return Err(HkError::invalid(
                "pin data must be an object of node name -> items",
            ));
        }
        raw.as_object_mut()
            .ok_or_else(|| HkError::invalid("workflow must be a JSON object"))?
            .insert("pinData".into(), pin);
    }
    let wf = Workflow::from_json_value(raw.clone())
        .map_err(|e| HkError::invalid(format!("invalid workflow: {e}")))?;
    wf.validate()
        .map_err(|e| HkError::invalid(format!("invalid workflow: {e}")))?;
    let start = match trigger_node {
        Some(n) => match wf.node(n) {
            Some(node) if !node.is_disabled() => node.name.clone(),
            _ => {
                return Err(HkError::invalid(format!(
                    "trigger node `{n}` is not an enabled node"
                )));
            }
        },
        None => find_trigger(&wf, TriggerKind::Manual)
            .or_else(|| {
                wf.nodes.iter().find(|n| {
                    !n.is_disabled()
                        && registry
                            .latest(&n.node_type)
                            .is_some_and(|i| i.description().is_trigger())
                })
            })
            .map(|n| n.name.clone())
            .ok_or_else(|| HkError::invalid("workflow has no trigger node to start from"))?,
    };
    let items = match input_items_json.filter(|s| !s.trim().is_empty()) {
        None => vec![Item::default()],
        Some(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(v)) if v.len() <= MAX_INPUT_ITEMS && v.iter().all(Value::is_object) => {
                v.iter().map(Item::from_pinned).collect()
            }
            Ok(_) => {
                return Err(HkError::invalid(format!(
                    "input items must be an array of up to {MAX_INPUT_ITEMS} objects"
                )));
            }
            Err(e) => return Err(HkError::invalid(format!("input items JSON: {e}"))),
        },
    };
    let name = raw
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .unwrap_or("Untitled")
        .to_owned();
    let workflow_id = raw
        .get("id")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty())
        .map(str::to_owned);
    Ok(Plan {
        wf,
        raw,
        start,
        items,
        name,
        workflow_id,
    })
}

impl LocalFlows {
    pub(crate) fn new(
        data_dir: &Path,
        credentials: Arc<dyn hk_flow::CredentialResolver>,
    ) -> Result<Self, HkError> {
        Ok(Self {
            s: Arc::new(Shared {
                dir: data_dir.join("flow_runs"),
                credentials,
                live: Mutex::new(LiveRuns::default()),
                seq: AtomicU64::new(0),
            }),
        })
    }

    /// Validate, register the run and execute it in the background. Needs a tokio runtime.
    pub(crate) fn start(
        &self,
        workflow_json: &str,
        mode: FlowRunModeDto,
        trigger_node: Option<&str>,
        input_items_json: Option<&str>,
        pin_data_json: Option<&str>,
    ) -> Result<String, HkError> {
        let plan = plan(
            workflow_json,
            trigger_node,
            input_items_json,
            pin_data_json,
            &crate::nodes::registry(),
        )?;
        let id = format!(
            "run-{}-{}",
            now_ms(),
            self.s.seq.fetch_add(1, Ordering::Relaxed)
        );
        let live = Live::new();
        {
            let mut g = lock(&self.s.live);
            g.by_id.insert(id.clone(), live.clone());
            g.order.push_back(id.clone());
            while g.order.len() > KEEP_LIVE {
                // Only finished runs may be evicted; a running one stays followable.
                let Some(pos) = g
                    .order
                    .iter()
                    .position(|k| g.by_id.get(k).is_none_or(|l| l.finished()))
                else {
                    break;
                };
                if let Some(k) = g.order.remove(pos) {
                    g.by_id.remove(&k);
                }
            }
        }
        let shared = self.s.clone();
        let run_id = id.clone();
        tokio::spawn(async move {
            // Own task so a panicking node still ends the stream.
            let inner = tokio::spawn(execute(
                shared.clone(),
                run_id.clone(),
                live.clone(),
                plan,
                mode,
            ));
            if inner.await.is_err() {
                let t = now_ms();
                live.push(terminal(
                    &run_id,
                    "error",
                    Some("run crashed".into()),
                    None,
                    t,
                    t,
                ));
            }
        });
        Ok(id)
    }

    pub(crate) fn events(&self, id: &str) -> Result<BoxStream<'static, FlowRunEventDto>, HkError> {
        if let Some(live) = lock(&self.s.live).by_id.get(id).cloned() {
            return Ok(live.follow());
        }
        let d = self.read(id)?;
        Ok(stream::iter(events_from_detail(&d)).boxed())
    }

    fn read(&self, id: &str) -> Result<FlowRunDetailDto, HkError> {
        if !valid_id(id) {
            return Err(HkError::new(
                HkErrorKind::NotFound,
                format!("run {id} not found"),
            ));
        }
        let bytes = std::fs::read(self.s.dir.join(format!("{id}.json")))
            .map_err(|_| HkError::new(HkErrorKind::NotFound, format!("run {id} not found")))?;
        let st: StoredRun = serde_json::from_slice(&bytes)?;
        Ok(FlowRunDetailDto {
            summary: st.meta.dto(),
            run_data_json: st.run_data.to_string(),
            workflow_json: st.workflow.map(|w| w.to_string()),
        })
    }

    pub(crate) async fn get(&self, id: &str) -> Result<FlowRunDetailDto, HkError> {
        let this = Self { s: self.s.clone() };
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || this.read(&id))
            .await
            .map_err(|e| HkError::internal(format!("join: {e}")))?
    }

    pub(crate) async fn list(
        &self,
        workflow_id: Option<String>,
        limit: u32,
    ) -> Result<Vec<FlowRunSummaryDto>, HkError> {
        let dir = self.s.dir.clone();
        let metas = tokio::task::spawn_blocking(move || read_metas(&dir))
            .await
            .map_err(|e| HkError::internal(format!("join: {e}")))?;
        Ok(metas
            .iter()
            .filter(|m| {
                workflow_id
                    .as_ref()
                    .is_none_or(|w| m.workflow_id.as_ref() == Some(w))
            })
            .take(limit as usize)
            .map(Meta::dto)
            .collect())
    }
}

async fn execute(
    shared: Arc<Shared>,
    id: String,
    live: Arc<Live>,
    plan: Plan,
    mode: FlowRunModeDto,
) {
    let started = now_ms();
    let mut first = FlowRunEventDto::new(&id, K::RunStarted);
    first.started_ms = started;
    first.node = Some(plan.start.clone());
    live.push(first);

    let store = Arc::new(LiveStore {
        run_id: id.clone(),
        live: live.clone(),
        seen: Mutex::new(HashMap::new()),
    });
    let cfg = ExecutorConfig {
        mode: "manual".into(),
        use_pin_data: mode == FlowRunModeDto::Test,
        ..ExecutorConfig::default()
    };
    let mut cfg = cfg;
    cfg.hooks.credentials = Some(shared.credentials.clone());
    if let Ok(link) = crate::n8n_sync::get() {
        cfg.hooks.remote_nodes = link.delegate().await;
    }
    // Registry is read per run so plugin installs apply to the next run without a restart.
    let exec = Executor::new(crate::nodes::registry())
        .with_store(store)
        .with_config(cfg);
    let mut input = ExecuteInput::new(plan.start.clone(), plan.items.clone());
    input.execution_id = Some(id.clone());

    let out = tokio::time::timeout(RUN_LIMIT, exec.execute(&plan.wf, &input)).await;
    let (status, error, err_node, mut run_data) = match out {
        Ok(Ok(res)) => {
            let status = match res.status {
                RunStatus::Success => "success",
                RunStatus::Error => "error",
                RunStatus::Waiting => "waiting",
                RunStatus::Running => "running",
            };
            let rd = serde_json::to_value(&res.run_data).unwrap_or_else(|_| json!({}));
            (
                status,
                res.error.as_ref().map(|e| e.message.clone()),
                res.error.and_then(|e| e.node),
                rd,
            )
        }
        Ok(Err(e)) => (
            "error",
            Some(e.to_string()),
            None,
            Value::Object(Map::new()),
        ),
        Err(_) => (
            "error",
            Some("run exceeded the time limit and was abandoned".into()),
            None,
            Value::Object(Map::new()),
        ),
    };
    let ended = now_ms();
    shrink_run_data(&mut run_data, DETAIL_MAX_ITEMS, DETAIL_MAX_BYTES);
    let mut workflow = plan.raw;
    scrub_headers(&mut workflow);
    let stored = StoredRun {
        meta: Meta {
            id: id.clone(),
            workflow_id: plan.workflow_id,
            workflow_name: plan.name,
            mode: if mode == FlowRunModeDto::Test {
                "test"
            } else {
                "manual"
            }
            .into(),
            status: status.into(),
            started_ms: started,
            finished_ms: Some(ended),
            error: error.clone(),
            node_count: u32::try_from(run_data.as_object().map_or(0, Map::len)).unwrap_or(u32::MAX),
        },
        run_data,
        workflow: Some(workflow),
    };
    let dir = shared.dir.clone();
    // Persisting is best effort: the run itself succeeded or failed on its own merits.
    match tokio::task::spawn_blocking(move || persist(&dir, &stored)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, run = %id, "cannot persist flow run"),
        Err(e) => tracing::warn!(error = %e, run = %id, "persist task failed"),
    }
    live.push(terminal(&id, status, error, err_node, started, ended));
}

// ---------------------------------------------------------------------------- remote helpers

/// `2026-01-02T03:04:05.678Z` -> unix ms (inverse of the server's formatter).
pub(crate) fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let ms = s
        .get(19..)
        .and_then(|r| r.strip_prefix('.'))
        .map(|r| {
            r.chars()
                .take_while(char::is_ascii_digit)
                .take(3)
                .collect::<String>()
        })
        .filter(|f| !f.is_empty())
        .map_or(0, |f| {
            f.parse::<i64>().unwrap_or(0) * 10_i64.pow(3 - u32::try_from(f.len()).unwrap_or(3))
        });
    // days-from-civil
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + hh * 3600 + mm * 60 + ss) * 1000) + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_round_trips_known_instants() {
        assert_eq!(parse_iso("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_iso("2026-01-02T03:04:05.678Z"),
            Some(1_767_323_045_678)
        );
        assert_eq!(parse_iso("2024-02-29T12:00:00Z"), Some(1_709_208_000_000));
        assert_eq!(parse_iso("nope"), None);
    }

    #[test]
    fn shrink_cuts_items_strips_binary_and_blanks_headers() {
        let mut main = json!([[
            {"json": {"headers": {"Authorization": "Bearer x", "ok": "1"}}, "binary": {"f": {"data": "AAAA", "mimeType": "a/b"}}},
            {"json": {"n": 2}}, {"json": {"n": 3}}
        ], null]);
        let (counts, cut) = shrink_main(&mut main, 2, 1 << 20);
        assert_eq!(counts, vec![3, 0]);
        assert!(cut);
        assert_eq!(main[0].as_array().map(Vec::len), Some(2));
        assert_eq!(main[0][0]["json"]["headers"]["Authorization"], "[redacted]");
        assert_eq!(main[0][0]["json"]["headers"]["ok"], "1");
        assert!(main[0][0]["binary"]["f"].get("data").is_none());
        assert_eq!(main[0][0]["binary"]["f"]["dataOmitted"], true);
    }
}
