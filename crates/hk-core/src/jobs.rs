//! Unified job model (DESIGN §3.14, D17): every long-running operation is a `Job` in one
//! tree, so progress, cancel and the activity UI work the same for agent turns, tools,
//! subagents, flows, renders and downloads.
//!
//! Cancel is cooperative first (each job's `CancellationToken`, a child of its parent's), then
//! forced: a job still running when its grace period ends gets its force callback run.
// port: dsh packages/core (ctx.jobs idea: job tree, cancel token, progress events)

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::error::CoreError;
use crate::host::{BoxFuture, Ctx, Plugin};

/// Grace between a cancel request and the force callback.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(5);
/// At most 10 progress events per second per job.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const EVENT_CAPACITY: usize = 1024;
/// Finished root trees kept for `list()` (late UIs); older ones are pruned.
const RETAIN_ROOTS: usize = 64;
const FORCE_LOG: &str = "grace period expired; forcing stop";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub u64);

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "job-{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    AgentTurn,
    Step,
    Tool,
    Subagent,
    FlowExecution,
    Render,
    Transcribe,
    Download,
    Index,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Progress {
    /// Fraction done, clamped to `0.0..=1.0`.
    Determinate(f32),
    #[default]
    Indeterminate,
}

impl Progress {
    fn clamped(self) -> Self {
        match self {
            Self::Determinate(p) if p.is_nan() => Self::Determinate(0.0),
            Self::Determinate(p) => Self::Determinate(p.clamp(0.0, 1.0)),
            Self::Indeterminate => Self::Indeterminate,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    /// Cancel requested; the job has not stopped yet.
    Cancelling,
    Cancelled,
    Finished,
    Failed {
        error: String,
    },
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Cancelled | Self::Finished | Self::Failed { .. })
    }
}

/// Snapshot of one job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: JobId,
    pub parent: Option<JobId>,
    pub kind: JobKind,
    pub title: String,
    /// Owner-defined lookup key (e.g. `tool:<session>:<call>`), see [`JobRegistry::find_key`].
    pub key: Option<String>,
    pub phase: Option<String>,
    pub progress: Progress,
    pub eta_ms: Option<u64>,
    pub started_at_ms: u64,
    pub meta: Value,
    pub state: JobState,
    /// Latest live metrics (LLM: ttft_ms, tokens_per_s, tokens_out, cost_usd, context_tokens …).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub metrics: Value,
}

/// What to start.
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub kind: JobKind,
    pub title: String,
    pub key: Option<String>,
    pub meta: Value,
    /// Overrides the registry grace for this job.
    pub grace: Option<Duration>,
}

impl JobSpec {
    pub fn new(kind: JobKind, title: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            key: None,
            meta: Value::Null,
            grace: None,
        }
    }

    #[must_use]
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    #[must_use]
    pub fn meta(mut self, meta: Value) -> Self {
        self.meta = meta;
        self
    }

    #[must_use]
    pub fn grace(mut self, grace: Duration) -> Self {
        self.grace = Some(grace);
        self
    }
}

/// Live job events; wire names are `job/<action>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)] // events are short-lived broadcast values
pub enum JobEvent {
    #[serde(rename = "job/started")]
    Started { job: JobInfo },
    #[serde(rename = "job/phase")]
    Phase { id: JobId, phase: String },
    #[serde(rename = "job/progress")]
    Progress {
        id: JobId,
        progress: Progress,
        eta_ms: Option<u64>,
    },
    #[serde(rename = "job/log")]
    Log { id: JobId, line: String },
    #[serde(rename = "job/metrics")]
    Metrics { id: JobId, metrics: Value },
    #[serde(rename = "job/cancel-requested")]
    CancelRequested { id: JobId },
    #[serde(rename = "job/cancelled")]
    Cancelled { id: JobId },
    #[serde(rename = "job/finished")]
    Finished { id: JobId },
    #[serde(rename = "job/failed")]
    Failed { id: JobId, error: String },
}

impl JobEvent {
    pub fn id(&self) -> JobId {
        match self {
            Self::Started { job } => job.id,
            Self::Phase { id, .. }
            | Self::Progress { id, .. }
            | Self::Log { id, .. }
            | Self::Metrics { id, .. }
            | Self::CancelRequested { id }
            | Self::Cancelled { id }
            | Self::Finished { id }
            | Self::Failed { id, .. } => *id,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Started { .. } => "job/started",
            Self::Phase { .. } => "job/phase",
            Self::Progress { .. } => "job/progress",
            Self::Log { .. } => "job/log",
            Self::Metrics { .. } => "job/metrics",
            Self::CancelRequested { .. } => "job/cancel-requested",
            Self::Cancelled { .. } => "job/cancelled",
            Self::Finished { .. } => "job/finished",
            Self::Failed { .. } => "job/failed",
        }
    }
}

type ForceFn = Box<dyn FnOnce() + Send>;

struct Entry {
    info: JobInfo,
    token: CancellationToken,
    children: Vec<JobId>,
    grace: Duration,
    force: Option<ForceFn>,
    started: Instant,
    last_progress: Option<Instant>,
    last_metrics: Option<Instant>,
    /// A throttled progress value not yet emitted.
    progress_dirty: bool,
}

#[derive(Default)]
struct State {
    jobs: HashMap<JobId, Entry>,
    next: u64,
    done_roots: VecDeque<JobId>,
}

struct Inner {
    state: Mutex<State>,
    tx: broadcast::Sender<JobEvent>,
    grace: Duration,
}

/// Job tree service. Cheap to clone; provide it on a `Host` with [`JobsPlugin`].
#[derive(Clone)]
pub struct JobRegistry(Arc<Inner>);

impl std::fmt::Debug for JobRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobRegistry")
            .field("jobs", &self.lock().jobs.len())
            .field("grace", &self.0.grace)
            .finish()
    }
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::with_grace(DEFAULT_GRACE)
    }

    pub fn with_grace(grace: Duration) -> Self {
        let (tx, _) = broadcast::channel(EVENT_CAPACITY);
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                next: 1,
                ..State::default()
            }),
            tx,
            grace,
        }))
    }

    pub fn grace(&self) -> Duration {
        self.0.grace
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // Sent under the state lock so subscribers see events in mutation order.
    fn emit(&self, ev: JobEvent) {
        let _ = self.0.tx.send(ev);
    }

    /// Start a job, nested under `parent` when that job is still known. A child of a
    /// cancelled parent starts out cancelled.
    pub fn start(&self, spec: JobSpec, parent: Option<JobId>) -> JobHandle {
        let mut st = self.lock();
        let id = JobId(st.next);
        st.next += 1;
        let parent = parent.filter(|p| st.jobs.contains_key(p));
        let token = match parent.and_then(|p| st.jobs.get_mut(&p)) {
            Some(p) => {
                p.children.push(id);
                p.token.child_token()
            }
            None => CancellationToken::new(),
        };
        let born_cancelled = token.is_cancelled();
        let grace = spec.grace.unwrap_or(self.0.grace);
        let info = JobInfo {
            id,
            parent,
            kind: spec.kind,
            title: spec.title,
            key: spec.key,
            phase: None,
            progress: Progress::Indeterminate,
            eta_ms: None,
            started_at_ms: now_ms(),
            meta: spec.meta,
            metrics: Value::Null,
            state: if born_cancelled {
                JobState::Cancelling
            } else {
                JobState::Running
            },
        };
        self.emit(JobEvent::Started { job: info.clone() });
        if born_cancelled {
            self.emit(JobEvent::CancelRequested { id });
        }
        st.jobs.insert(
            id,
            Entry {
                info,
                token: token.clone(),
                children: Vec::new(),
                grace,
                force: None,
                started: Instant::now(),
                last_progress: None,
                last_metrics: None,
                progress_dirty: false,
            },
        );
        drop(st);
        if born_cancelled {
            self.arm(vec![(id, grace)]);
        }
        JobHandle {
            reg: self.clone(),
            id,
            token,
            done: false,
        }
    }

    /// Request cancel of `id` and its subtree. `false` when the job is unknown or already
    /// ended (a finished job ignores cancel).
    pub fn cancel(&self, id: JobId) -> bool {
        let mut st = self.lock();
        let Some(root) = st.jobs.get(&id) else {
            return false;
        };
        if root.info.state.is_terminal() {
            return false;
        }
        let root_token = root.token.clone();
        let mut armed = Vec::new();
        // Pre-order: a parent's cancel-requested precedes its children's.
        let mut stack = vec![id];
        while let Some(j) = stack.pop() {
            let Some(e) = st.jobs.get_mut(&j) else {
                continue;
            };
            stack.extend(e.children.iter().rev());
            if e.info.state == JobState::Running {
                e.info.state = JobState::Cancelling;
                armed.push((j, e.grace));
                self.emit(JobEvent::CancelRequested { id: j });
            }
        }
        // Children hold child tokens, so this reaches the whole subtree.
        root_token.cancel();
        drop(st);
        self.arm(armed);
        true
    }

    fn arm(&self, jobs: Vec<(JobId, Duration)>) {
        if jobs.is_empty() {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("job cancel outside a tokio runtime; force callbacks not armed");
            return;
        };
        for (id, grace) in jobs {
            let reg = self.clone();
            rt.spawn(async move {
                tokio::time::sleep(grace).await;
                reg.force(id);
            });
        }
    }

    fn force(&self, id: JobId) {
        let f = {
            let mut st = self.lock();
            let Some(e) = st.jobs.get_mut(&id) else {
                return;
            };
            if e.info.state.is_terminal() {
                return;
            }
            let Some(f) = e.force.take() else {
                return;
            };
            self.emit(JobEvent::Log {
                id,
                line: FORCE_LOG.to_owned(),
            });
            f
        };
        tracing::debug!(job = %id, "forcing job stop");
        f();
    }

    pub fn get(&self, id: JobId) -> Option<JobInfo> {
        self.lock().jobs.get(&id).map(|e| e.info.clone())
    }

    /// All known jobs (running plus recently ended), ordered by id.
    pub fn list(&self) -> Vec<JobInfo> {
        let mut v: Vec<JobInfo> = self.lock().jobs.values().map(|e| e.info.clone()).collect();
        v.sort_by_key(|j| j.id);
        v
    }

    /// Cancel every unfinished root job (and so every subtree). Returns how many were hit.
    pub fn cancel_all(&self) -> usize {
        let roots: Vec<JobId> = self
            .lock()
            .jobs
            .values()
            .filter(|e| e.info.parent.is_none() && !e.info.state.is_terminal())
            .map(|e| e.info.id)
            .collect();
        roots.into_iter().filter(|id| self.cancel(*id)).count()
    }

    /// Newest unfinished job started with `key`.
    pub fn find_key(&self, key: &str) -> Option<JobId> {
        self.lock()
            .jobs
            .values()
            .filter(|e| !e.info.state.is_terminal() && e.info.key.as_deref() == Some(key))
            .map(|e| e.info.id)
            .max()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<JobEvent> {
        self.0.tx.subscribe()
    }

    /// Runs `f` on a live entry; ended or pruned jobs are left alone.
    fn with_live(&self, id: JobId, f: impl FnOnce(&Self, &mut Entry)) {
        let mut st = self.lock();
        if let Some(e) = st.jobs.get_mut(&id)
            && !e.info.state.is_terminal()
        {
            f(self, e);
        }
    }

    fn flush_progress(&self, e: &mut Entry) {
        if e.progress_dirty {
            e.progress_dirty = false;
            e.last_progress = Some(Instant::now());
            self.emit(JobEvent::Progress {
                id: e.info.id,
                progress: e.info.progress,
                eta_ms: e.info.eta_ms,
            });
        }
    }

    fn end(&self, id: JobId, state: JobState) {
        let mut st = self.lock();
        let Some(e) = st.jobs.get_mut(&id) else {
            return;
        };
        if e.info.state.is_terminal() {
            return;
        }
        self.flush_progress(e);
        e.force = None;
        e.info.eta_ms = None;
        let ev = match &state {
            JobState::Finished => JobEvent::Finished { id },
            JobState::Failed { error } => JobEvent::Failed {
                id,
                error: error.clone(),
            },
            _ => JobEvent::Cancelled { id },
        };
        e.info.state = state;
        let is_root = e.info.parent.is_none();
        self.emit(ev);
        if is_root {
            st.done_roots.push_back(id);
            while st.done_roots.len() > RETAIN_ROOTS {
                if let Some(old) = st.done_roots.pop_front() {
                    prune(&mut st.jobs, old);
                }
            }
        }
    }
}

fn prune(jobs: &mut HashMap<JobId, Entry>, root: JobId) {
    let mut stack = vec![root];
    while let Some(j) = stack.pop() {
        if let Some(e) = jobs.remove(&j) {
            stack.extend(e.children);
        }
    }
}

/// Owner side of a running job. Dropping it without an end call records the job as
/// cancelled (token cancelled) or failed, so the tree never keeps stale running jobs.
pub struct JobHandle {
    reg: JobRegistry,
    id: JobId,
    token: CancellationToken,
    done: bool,
}

impl std::fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobHandle")
            .field("id", &self.id)
            .field("cancelled", &self.token.is_cancelled())
            .finish()
    }
}

impl JobHandle {
    pub fn id(&self) -> JobId {
        self.id
    }

    /// Cooperative cancel signal; also cancelled when any ancestor is.
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub fn registry(&self) -> &JobRegistry {
        &self.reg
    }

    /// Start a nested job.
    pub fn child(&self, spec: JobSpec) -> JobHandle {
        self.reg.start(spec, Some(self.id))
    }

    pub fn phase(&self, phase: impl Into<String>) {
        let phase = phase.into();
        self.reg.with_live(self.id, |reg, e| {
            reg.flush_progress(e);
            e.info.phase = Some(phase.clone());
            reg.emit(JobEvent::Phase {
                id: e.info.id,
                phase,
            });
        });
    }

    /// Update progress; ETA is extrapolated from elapsed time for determinate values.
    pub fn progress(&self, p: Progress) {
        self.set_progress(p, None, true);
    }

    /// Update progress with an owner-known ETA.
    pub fn progress_with_eta(&self, p: Progress, eta: Option<Duration>) {
        self.set_progress(p, eta, false);
    }

    fn set_progress(&self, p: Progress, eta: Option<Duration>, auto_eta: bool) {
        let p = p.clamped();
        self.reg.with_live(self.id, |reg, e| {
            let now = Instant::now();
            e.info.progress = p;
            e.info.eta_ms = match (auto_eta, p) {
                (true, Progress::Determinate(f)) if f > 0.0 && f < 1.0 => {
                    let elapsed = now.duration_since(e.started).as_secs_f64();
                    Some((elapsed * f64::from(1.0 - f) / f64::from(f) * 1000.0) as u64)
                }
                (true, _) => None,
                (false, _) => eta.map(|d| d.as_millis() as u64),
            };
            let complete = matches!(p, Progress::Determinate(f) if f >= 1.0);
            let due = e
                .last_progress
                .is_none_or(|t| now.duration_since(t) >= PROGRESS_INTERVAL);
            e.progress_dirty = true;
            if due || complete {
                reg.flush_progress(e);
            }
        });
    }

    /// Replace the job's live metrics; emitted at most every 250 ms unless `force`.
    pub fn metrics(&self, metrics: Value, force: bool) {
        self.reg.with_live(self.id, |reg, e| {
            let now = Instant::now();
            e.info.metrics = metrics.clone();
            if force
                || e.last_metrics
                    .is_none_or(|t| now.duration_since(t) >= Duration::from_millis(250))
            {
                e.last_metrics = Some(now);
                reg.emit(JobEvent::Metrics {
                    id: e.info.id,
                    metrics,
                });
            }
        });
    }

    pub fn log(&self, line: impl Into<String>) {
        let line = line.into();
        self.reg.with_live(self.id, |reg, e| {
            reg.flush_progress(e);
            reg.emit(JobEvent::Log {
                id: e.info.id,
                line,
            });
        });
    }

    /// Hard stop run if the job is still going when its grace period after a cancel request
    /// ends (drop an HTTP stream, kill a process group). Replaces an earlier callback.
    pub fn on_force(&self, f: impl FnOnce() + Send + 'static) {
        self.reg
            .with_live(self.id, |_, e| e.force = Some(Box::new(f)));
    }

    pub fn finish(mut self) {
        self.close(JobState::Finished);
    }

    pub fn fail(mut self, error: impl Into<String>) {
        self.close(JobState::Failed {
            error: error.into(),
        });
    }

    /// Record that the job stopped because of a cancel (partial work kept by the owner).
    pub fn cancelled(mut self) {
        self.close(JobState::Cancelled);
    }

    fn close(&mut self, state: JobState) {
        self.done = true;
        self.reg.end(self.id, state);
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        if !self.done {
            let state = if self.token.is_cancelled() {
                JobState::Cancelled
            } else {
                JobState::Failed {
                    error: "job handle dropped before completion".to_owned(),
                }
            };
            self.close(state);
        }
    }
}

/// Provides a [`JobRegistry`] service on the host.
#[derive(Debug, Clone)]
pub struct JobsPlugin {
    grace: Duration,
}

impl Default for JobsPlugin {
    fn default() -> Self {
        Self {
            grace: DEFAULT_GRACE,
        }
    }
}

impl JobsPlugin {
    pub fn new(grace: Duration) -> Self {
        Self { grace }
    }
}

impl Plugin for JobsPlugin {
    fn name(&self) -> &str {
        "jobs"
    }

    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<(), CoreError>> {
        Box::pin(async move {
            ctx.provide(Arc::new(JobRegistry::with_grace(self.grace)))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::host::Host;

    fn drain(rx: &mut broadcast::Receiver<JobEvent>) -> Vec<JobEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    fn names(evs: &[JobEvent]) -> Vec<&'static str> {
        evs.iter().map(JobEvent::name).collect()
    }

    #[tokio::test]
    async fn cancel_parent_cancels_children() {
        let reg = JobRegistry::new();
        let turn = reg.start(JobSpec::new(JobKind::AgentTurn, "turn"), None);
        let step = turn.child(JobSpec::new(JobKind::Step, "step"));
        let tool = step.child(JobSpec::new(JobKind::Tool, "tool"));
        let other = reg.start(JobSpec::new(JobKind::Download, "dl"), None);
        let mut rx = reg.subscribe();

        assert!(reg.cancel(turn.id()));
        assert!(turn.is_cancelled() && step.is_cancelled() && tool.is_cancelled());
        assert!(!other.is_cancelled());
        let evs = drain(&mut rx);
        let ids: Vec<JobId> = evs.iter().map(JobEvent::id).collect();
        assert_eq!(ids, vec![turn.id(), step.id(), tool.id()]);
        assert!(names(&evs).iter().all(|n| *n == "job/cancel-requested"));
        assert_eq!(reg.get(tool.id()).unwrap().state, JobState::Cancelling);

        // A job started under a cancelled parent is born cancelled.
        let late = step.child(JobSpec::new(JobKind::Tool, "late"));
        assert!(late.is_cancelled());
        drop(late);
        let tool_id = tool.id();
        tool.cancelled();
        assert_eq!(reg.get(tool_id).unwrap().state, JobState::Cancelled);
    }

    #[tokio::test]
    async fn grace_expiry_triggers_force_callback() {
        let reg = JobRegistry::with_grace(Duration::from_millis(20));
        let stuck = reg.start(JobSpec::new(JobKind::Render, "stuck"), None);
        let polite = reg.start(JobSpec::new(JobKind::Render, "polite"), None);
        let (forced, spared) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let f = forced.clone();
        stuck.on_force(move || f.store(true, Ordering::SeqCst));
        let s = spared.clone();
        polite.on_force(move || s.store(true, Ordering::SeqCst));
        let mut rx = reg.subscribe();

        reg.cancel(stuck.id());
        reg.cancel(polite.id());
        polite.cancelled(); // stopped within grace
        tokio::time::sleep(Duration::from_millis(120)).await;

        assert!(forced.load(Ordering::SeqCst));
        assert!(!spared.load(Ordering::SeqCst));
        let evs = drain(&mut rx);
        assert!(evs.contains(&JobEvent::Log {
            id: stuck.id(),
            line: FORCE_LOG.to_owned()
        }));
    }

    #[tokio::test]
    async fn finished_job_ignores_cancel() {
        let reg = JobRegistry::with_grace(Duration::from_millis(5));
        let job = reg.start(JobSpec::new(JobKind::Index, "idx"), None);
        let token = job.token().clone();
        let id = job.id();
        job.finish();
        let mut rx = reg.subscribe();
        assert!(!reg.cancel(id));
        assert!(!token.is_cancelled());
        assert!(drain(&mut rx).is_empty());
        assert_eq!(reg.get(id).unwrap().state, JobState::Finished);
    }

    #[tokio::test]
    async fn event_order_and_progress_throttle() {
        let reg = JobRegistry::new();
        let mut rx = reg.subscribe();
        let job = reg.start(
            JobSpec::new(JobKind::Transcribe, "t").meta(serde_json::json!({"file": "a.wav"})),
            None,
        );
        job.phase("decode");
        for i in 0..50 {
            job.progress(Progress::Determinate(i as f32 / 100.0));
        }
        job.log("chunk 1");
        let id = job.id();
        job.finish();

        let evs = drain(&mut rx);
        assert_eq!(
            names(&evs),
            vec![
                "job/started",
                "job/phase",
                "job/progress",
                "job/progress", // throttled tail flushed before the log line
                "job/log",
                "job/finished"
            ]
        );
        assert!(matches!(
            evs[3],
            JobEvent::Progress { id: i, progress: Progress::Determinate(p), eta_ms: Some(_) }
                if i == id && (p - 0.49).abs() < 1e-6
        ));
        // Ended jobs ignore further updates.
        assert!(drain(&mut rx).is_empty());
        let wire = serde_json::to_value(&evs[5]).unwrap();
        assert_eq!(wire["type"], "job/finished");
    }

    #[tokio::test]
    async fn dropped_handle_fails_and_plugin_provides_registry() {
        let host = Host::new();
        host.load(Arc::new(JobsPlugin::default())).await.unwrap();
        let reg = host.services().require::<JobRegistry>(None).unwrap();
        let id = reg
            .start(JobSpec::new(JobKind::Other("x".into()), "x"), None)
            .id();
        assert!(matches!(
            reg.get(id).unwrap().state,
            JobState::Failed { .. }
        ));
        assert_eq!(reg.find_key("nope"), None);
    }
}
