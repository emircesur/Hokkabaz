//! Trigger runtime: arms schedules, webhooks, app-event and file-watch triggers of active
//! workflows and reports every firing as a [`TriggerEvent`].
//!
//! hk-flow does not run the workflow itself: the daemon/server gives the host a
//! [`TriggerSink`] and turns each event into an `Executor` run (it owns workflow storage,
//! the executor config and the run store). That keeps this crate free of a runtime policy.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use chrono_tz::Tz;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::events::{AppEventBus, app_event_item};
use super::schedule::{ScheduleSpec, schedule_item, workflow_tz};
use super::webhook::{PreparedWebhook, WebhookKind, WebhookRegistry, WebhookRoute};
use super::{TriggerSpec, error_trigger_item, iso_at, trigger_spec};
use crate::error::FlowError;
use crate::executor::{ExecutionResult, Executor};
use crate::item::Item;
use crate::nodes::date::now_millis;
use crate::run_data::RunStatus;
use crate::workflow::Workflow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMode {
    /// Schedule, app event, file watch (`$execution.mode == "trigger"`).
    Trigger,
    Webhook,
    Error,
}

impl TriggerMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trigger => "trigger",
            Self::Webhook => "webhook",
            Self::Error => "error",
        }
    }
}

/// "Run this workflow from this trigger with these items."
#[derive(Debug, Clone)]
pub struct TriggerEvent {
    pub workflow_id: String,
    /// Start node. `None` = the workflow's Error Trigger (error workflows).
    pub node_name: Option<String>,
    pub items: Vec<Item>,
    pub mode: TriggerMode,
}

/// Where fired triggers go. Must not block: spawn or queue.
pub trait TriggerSink: Send + Sync + fmt::Debug {
    fn emit(&self, event: TriggerEvent);
}

/// Sink over an unbounded channel; the daemon drains the receiver.
#[derive(Debug, Clone)]
pub struct ChannelSink(mpsc::UnboundedSender<TriggerEvent>);

impl ChannelSink {
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<TriggerEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(tx), rx)
    }
}

impl TriggerSink for ChannelSink {
    fn emit(&self, event: TriggerEvent) {
        // receiver dropped = daemon shutting down
        let _ = self.0.send(event);
    }
}

/// Sink over a plain callback.
pub struct FnSink(Box<dyn Fn(TriggerEvent) + Send + Sync>);

impl FnSink {
    pub fn new(f: impl Fn(TriggerEvent) + Send + Sync + 'static) -> Self {
        Self(Box::new(f))
    }
}

impl fmt::Debug for FnSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FnSink")
    }
}

impl TriggerSink for FnSink {
    fn emit(&self, event: TriggerEvent) {
        (self.0)(event);
    }
}

/// What activating a workflow armed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Activation {
    pub webhooks: Vec<WebhookRoute>,
    pub schedules: usize,
    pub app_events: usize,
    pub file_watches: usize,
}

/// Activate/deactivate workflows. Implemented by [`LocalTriggerHost`]; the daemon may wrap
/// or replace it (e.g. a clustered host).
#[async_trait]
pub trait TriggerHost: Send + Sync + fmt::Debug {
    async fn activate(&self, workflow: &Workflow) -> Result<Activation, FlowError>;
    /// No-op for a workflow that is not active.
    async fn deactivate(&self, workflow_id: &str) -> Result<(), FlowError>;
    fn is_active(&self, workflow_id: &str) -> bool;
}

// ------------------------------------------------------------------- local host

#[derive(Debug)]
struct Armed {
    node: String,
    spec: ScheduleSpec,
    tz: Tz,
    anchor_ms: i64,
    next: Option<i64>,
}

#[derive(Default)]
struct ActiveWorkflow {
    schedules: Vec<Armed>,
    tasks: Vec<JoinHandle<()>>,
    /// OS watchers etc.; dropped on deactivate. Unused without the `notify` feature.
    #[cfg_attr(
        not(all(feature = "notify", not(target_os = "android"))),
        allow(dead_code)
    )]
    guards: Vec<Box<dyn Send>>,
}

impl Drop for ActiveWorkflow {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

pub struct LocalTriggerHost {
    sink: Arc<dyn TriggerSink>,
    webhooks: Arc<WebhookRegistry>,
    bus: AppEventBus,
    state: Mutex<BTreeMap<String, ActiveWorkflow>>,
    /// Fires overdue by more than this are dropped, not replayed (restart, suspend).
    grace_ms: i64,
}

impl fmt::Debug for LocalTriggerHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalTriggerHost")
            .field("active", &self.active_ids())
            .finish_non_exhaustive()
    }
}

impl LocalTriggerHost {
    pub fn new(sink: Arc<dyn TriggerSink>, webhooks: Arc<WebhookRegistry>) -> Self {
        Self {
            sink,
            webhooks,
            bus: AppEventBus::default(),
            state: Mutex::new(BTreeMap::new()),
            grace_ms: 60_000,
        }
    }

    pub fn with_bus(mut self, bus: AppEventBus) -> Self {
        self.bus = bus;
        self
    }

    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace_ms = i64::try_from(grace.as_millis()).unwrap_or(i64::MAX);
        self
    }

    pub fn webhooks(&self) -> &Arc<WebhookRegistry> {
        &self.webhooks
    }

    pub fn bus(&self) -> &AppEventBus {
        &self.bus
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, ActiveWorkflow>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn active_ids(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    /// [`TriggerHost::activate`] with an explicit clock (tests, replays).
    pub async fn activate_at(&self, wf: &Workflow, now_ms: i64) -> Result<Activation, FlowError> {
        let id = wf
            .id
            .clone()
            .ok_or_else(|| FlowError::Invalid("workflow has no id".into()))?;
        let tz = workflow_tz(wf.settings.as_ref().and_then(|s| s.timezone.as_deref()));

        let mut specs = Vec::new();
        for n in wf.nodes.iter().filter(|n| !n.is_disabled()) {
            if let Some(s) = trigger_spec(n)? {
                specs.push((n.name.clone(), s));
            }
        }
        if !specs.iter().any(|(_, s)| !matches!(s, TriggerSpec::Manual)) {
            return Err(FlowError::Invalid(
                "workflow has no trigger node that can be activated".into(),
            ));
        }

        self.deactivate_inner(&id);
        let mut active = ActiveWorkflow::default();
        let mut out = Activation::default();
        // webhooks first: the only step that can clash with other workflows
        if specs
            .iter()
            .any(|(_, s)| matches!(s, TriggerSpec::Webhook(_)))
        {
            out.webhooks = self
                .webhooks
                .register_workflow(wf, WebhookKind::Production)?;
        }
        let armed = self.arm_rest(&id, tz, now_ms, &specs, &mut active, &mut out);
        if let Err(e) = armed {
            self.webhooks
                .unregister_workflow(&id, WebhookKind::Production);
            return Err(e);
        }
        self.lock().insert(id, active);
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn arm_rest(
        &self,
        id: &str,
        tz: Tz,
        now_ms: i64,
        specs: &[(String, TriggerSpec)],
        active: &mut ActiveWorkflow,
        out: &mut Activation,
    ) -> Result<(), FlowError> {
        for (node, spec) in specs {
            match spec {
                TriggerSpec::Schedule(s) => {
                    active.schedules.push(Armed {
                        node: node.clone(),
                        next: s.next_fire(now_ms, tz, now_ms),
                        spec: s.clone(),
                        tz,
                        anchor_ms: now_ms,
                    });
                    out.schedules += 1;
                }
                TriggerSpec::AppEvent(s) => {
                    let mut rx = self.bus.subscribe();
                    let (sink, wf_id, node, s) = (
                        Arc::clone(&self.sink),
                        id.to_owned(),
                        node.clone(),
                        s.clone(),
                    );
                    active.tasks.push(tokio::spawn(async move {
                        use tokio::sync::broadcast::error::RecvError;
                        loop {
                            match rx.recv().await {
                                Ok(ev) if s.matches(&ev.name) => sink.emit(TriggerEvent {
                                    workflow_id: wf_id.clone(),
                                    node_name: Some(node.clone()),
                                    items: vec![app_event_item(&ev, iso_at(now_millis()))],
                                    mode: TriggerMode::Trigger,
                                }),
                                Ok(_) | Err(RecvError::Lagged(_)) => {}
                                Err(RecvError::Closed) => break,
                            }
                        }
                    }));
                    out.app_events += 1;
                }
                TriggerSpec::FileWatch(s) => {
                    self.arm_file_watch(id, node, s, active)?;
                    out.file_watches += 1;
                }
                _ => {}
            }
        }
        Ok(())
    }

    #[cfg(all(feature = "notify", not(target_os = "android")))]
    fn arm_file_watch(
        &self,
        id: &str,
        node: &str,
        spec: &super::events::FileWatchSpec,
        active: &mut ActiveWorkflow,
    ) -> Result<(), FlowError> {
        let (watcher, mut rx) = super::events::watch::start(spec)?;
        active.guards.push(Box::new(watcher));
        let (sink, wf_id, node) = (Arc::clone(&self.sink), id.to_owned(), node.to_owned());
        active.tasks.push(tokio::spawn(async move {
            while let Some((kind, path)) = rx.recv().await {
                sink.emit(TriggerEvent {
                    workflow_id: wf_id.clone(),
                    node_name: Some(node.clone()),
                    items: vec![super::events::file_event_item(
                        &kind,
                        &path,
                        iso_at(now_millis()),
                    )],
                    mode: TriggerMode::Trigger,
                });
            }
        }));
        Ok(())
    }

    #[cfg(not(all(feature = "notify", not(target_os = "android"))))]
    fn arm_file_watch(
        &self,
        _id: &str,
        node: &str,
        _spec: &super::events::FileWatchSpec,
        _active: &mut ActiveWorkflow,
    ) -> Result<(), FlowError> {
        Err(FlowError::Invalid(format!(
            "file watch trigger `{node}` needs the `notify` feature on a desktop target"
        )))
    }

    fn deactivate_inner(&self, id: &str) {
        self.lock().remove(id);
        self.webhooks
            .unregister_workflow(id, WebhookKind::Production);
    }

    /// Fire every schedule whose time has come. Returns how many fired. Fires overdue by
    /// more than the grace period are dropped (the next fire is computed from `now_ms`),
    /// so a restart or a laptop wake-up does not replay history.
    pub fn tick(&self, now_ms: i64) -> usize {
        let mut events = Vec::new();
        {
            let mut st = self.lock();
            for (wf_id, aw) in st.iter_mut() {
                for a in &mut aw.schedules {
                    let Some(due) = a.next else { continue };
                    if due > now_ms {
                        continue;
                    }
                    if now_ms - due <= self.grace_ms
                        && let Some(item) = schedule_item(due, a.tz)
                    {
                        events.push(TriggerEvent {
                            workflow_id: wf_id.clone(),
                            node_name: Some(a.node.clone()),
                            items: vec![item],
                            mode: TriggerMode::Trigger,
                        });
                    }
                    a.next = a.spec.next_fire(now_ms, a.tz, a.anchor_ms);
                }
            }
        }
        let n = events.len();
        for e in events {
            self.sink.emit(e);
        }
        n
    }

    /// Recompute every schedule's next fire from `now_ms` without firing anything.
    pub fn rearm(&self, now_ms: i64) {
        for aw in self.lock().values_mut() {
            for a in &mut aw.schedules {
                a.next = a.spec.next_fire(now_ms, a.tz, a.anchor_ms);
            }
        }
    }

    /// Earliest armed schedule fire (unix ms); lets a loop sleep exactly that long.
    pub fn next_fire_at(&self) -> Option<i64> {
        self.lock()
            .values()
            .flat_map(|w| w.schedules.iter().filter_map(|a| a.next))
            .min()
    }

    /// Hand a parsed webhook request to the sink (server calls this after
    /// [`WebhookRegistry::prepare`] when it does not run the execution itself).
    pub fn dispatch_webhook(&self, p: PreparedWebhook) {
        self.sink.emit(TriggerEvent {
            workflow_id: p.route.workflow_id,
            node_name: Some(p.route.node_name),
            items: vec![p.item],
            mode: TriggerMode::Webhook,
        });
    }

    /// Error workflows: call after every execution of `wf`. A failed run of a workflow
    /// with `settings.errorWorkflow` emits an Error Trigger event for that workflow
    /// (never for the failing workflow itself: no loops). Returns whether it emitted.
    pub fn on_execution_finished(
        &self,
        wf: &Workflow,
        res: &ExecutionResult,
        base_url: Option<&str>,
    ) -> bool {
        if res.status != RunStatus::Error {
            return false;
        }
        let Some(target) = wf
            .settings
            .as_ref()
            .and_then(|s| s.error_workflow.clone())
            .filter(|t| !t.is_empty() && Some(t) != wf.id.as_ref())
        else {
            return false;
        };
        self.sink.emit(TriggerEvent {
            workflow_id: target,
            node_name: None,
            items: vec![error_trigger_item(wf, res, "trigger", base_url)],
            mode: TriggerMode::Error,
        });
        true
    }
}

#[async_trait]
impl TriggerHost for LocalTriggerHost {
    async fn activate(&self, workflow: &Workflow) -> Result<Activation, FlowError> {
        self.activate_at(workflow, now_millis()).await
    }

    async fn deactivate(&self, workflow_id: &str) -> Result<(), FlowError> {
        self.deactivate_inner(workflow_id);
        Ok(())
    }

    fn is_active(&self, workflow_id: &str) -> bool {
        self.lock().contains_key(workflow_id)
    }
}

/// Background ticker: every `period` fire due schedules and (when an executor with a wait
/// store is given) resume due durable waits. `on_resumed` sees each resumed execution.
/// Abort the handle to stop. Call [`LocalTriggerHost::rearm`] first after a restart if the
/// host was populated long before the ticker starts.
pub fn spawn_ticker(
    host: Arc<LocalTriggerHost>,
    executor: Option<Executor>,
    period: Duration,
    on_resumed: impl Fn(Result<ExecutionResult, FlowError>) + Send + Sync + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(period);
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            iv.tick().await;
            let now = now_millis();
            host.tick(now);
            if let Some(ex) = &executor {
                for r in ex.resume_due(now).await {
                    on_resumed(r);
                }
            }
        }
    })
}
