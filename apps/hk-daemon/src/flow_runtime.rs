//! Flow runtime: glue between hk-server (webhook ingress, REST) and hk-flow (triggers,
//! executor). hk-server keeps its hk-flow dependency feature-light (no QuickJS); this is the
//! one place that runs workflows.
//!
//! - [`FlowRuntime::webhook_handler`]: `hk_server::WebhookHandler` over hk-flow's
//!   `WebhookRegistry`. A request becomes an execution row, a spawned `Executor` run and a
//!   result channel for `lastNode` / `responseNode` answers.
//! - [`FlowRuntime::activator`]: `hk_server::WorkflowActivator` over `LocalTriggerHost`
//!   (webhook, schedule, app-event, file-watch triggers).
//! - Trigger events (schedule etc.) are drained from a channel and run the same way.
//! - Durable waits park in SQLite; a ticker fires due schedules and resumes due waits, so a
//!   restart picks both up again ([`FlowRuntime::activate_stored`] re-arms schedules).
//!
//! Executions are recorded through `FlowApi::start_execution` / `finish_execution`, i.e. in
//! the same table the REST API reads, with the execution id shared with the executor.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use hk_flow::triggers::{
    self as ft, ChannelSink, LocalTriggerHost, TriggerEvent, TriggerHost, TriggerKind, WebhookKind,
};
use hk_flow::{
    CredentialRef, CredentialResolver, ExecuteInput, ExecutionResult, Executor, ExecutorConfig,
    FlowError, Item, NodeRegistry, ResponseSink, RunStatus, Workflow, register_core_nodes,
};
use hk_server as hs;
use hk_server::{ApiError, FlowApi, WebhookCall, WebhookMode, WebhookResult, WebhookRun};
use hk_store::{SecretStore, Store};
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::scrub::scrub_headers;
use crate::wait_store::SqliteWaitStore;

#[derive(Debug, Clone)]
pub struct FlowConfig {
    /// Host env var names readable through `$env`. Empty = none (host env can hold secrets).
    pub env_allowlist: Vec<String>,
    /// Public base of the listener (`https://host`): `webhookUrl`, `$execution.resumeUrl`.
    pub webhook_base_url: Option<String>,
    /// Scheduler / due-wait poll period.
    pub tick: Duration,
    /// Data dir; plugins are loaded from `<data_dir>/plugins`.
    pub data_dir: Option<std::path::PathBuf>,
    /// Backs `hokkabaz.agent`; `assemble` fills it with a session-backed runner when unset.
    pub agent_runner: Option<Arc<dyn hk_flow::nodes::ai::AgentRunner>>,
}

impl Default for FlowConfig {
    fn default() -> Self {
        Self {
            env_allowlist: Vec::new(),
            webhook_base_url: None,
            data_dir: None,
            tick: Duration::from_secs(1),
            agent_runner: None,
        }
    }
}

pub struct FlowRuntime {
    api: FlowApi,
    exec_cfg: ExecutorConfig,
    executor: Executor,
    nodes: Arc<NodeRegistry>,
    host: Arc<LocalTriggerHost>,
    /// Arms schedules/webhooks through `host` and, with integrations, polling triggers too.
    triggers: Arc<dyn TriggerHost>,
    #[cfg(feature = "plugins")]
    plugins: Option<crate::plugins::Loaded>,
    registry: Arc<ft::WebhookRegistry>,
    base_url: Option<String>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl fmt::Debug for FlowRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlowRuntime")
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// Execution row created, workflow loaded; the run itself comes next.
struct Started {
    wf: Workflow,
    exec_id: i64,
}

impl FlowRuntime {
    /// Build the executor and trigger host and start the background loops. `api` supplies the
    /// store, secrets and the execution event channel; wire the result into it with
    /// `api.with_webhooks(rt.webhook_handler()).with_activator(rt.activator())` (clones of a
    /// `FlowApi` share the event channel).
    pub fn start(api: FlowApi, cfg: FlowConfig) -> Result<Arc<Self>, FlowError> {
        let mut nodes = NodeRegistry::new();
        register_core_nodes(&mut nodes)?;
        #[cfg(feature = "integrations")]
        let mut polls = hk_integrations::PollRegistry::new();
        #[cfg(feature = "integrations")]
        hk_integrations::register_integrations_with_polling(
            &mut nodes,
            &hk_integrations::CredentialTypes::with_builtins(),
            &mut polls,
        )?;
        #[cfg(feature = "plugins")]
        let plugins = cfg
            .data_dir
            .as_deref()
            .map(|d| crate::plugins::load(d, api.secrets.clone(), &mut nodes, &mut polls));
        let exec_cfg = ExecutorConfig {
            mode: "trigger".into(),
            env_allowlist: cfg.env_allowlist,
            webhook_base_url: cfg.webhook_base_url.clone(),
            hooks: hk_flow::NodeHooks {
                credentials: Some(Arc::new(StoredCredentials {
                    store: api.store.clone(),
                    secrets: api.secrets.clone(),
                })),
                agent_runner: cfg.agent_runner,
                ..hk_flow::NodeHooks::default()
            },
            ..ExecutorConfig::default()
        };
        let nodes = Arc::new(nodes);
        let executor = Executor::new(nodes.clone())
            .with_wait_store(Arc::new(SqliteWaitStore::new(api.store.clone())))
            .with_config(exec_cfg.clone());

        let (sink, mut events) = ChannelSink::channel();
        let sink = Arc::new(sink);
        let registry = Arc::new(ft::WebhookRegistry::new());
        let host = Arc::new(LocalTriggerHost::new(sink.clone(), registry.clone()));
        #[cfg(feature = "integrations")]
        let triggers: Arc<dyn TriggerHost> = Arc::new(hk_integrations::PollingHost::new(
            host.clone(),
            polls,
            Arc::new(hk_integrations::poll::StaticDataPollState::new(
                StoreStatic {
                    store: api.store.clone(),
                },
            )),
            sink,
            exec_cfg.hooks.credentials.clone(),
        ));
        #[cfg(not(feature = "integrations"))]
        let triggers: Arc<dyn TriggerHost> = host.clone();
        let rt = Arc::new(Self {
            api,
            exec_cfg,
            executor: executor.clone(),
            nodes,
            host: host.clone(),
            triggers,
            #[cfg(feature = "plugins")]
            plugins,
            registry,
            base_url: cfg
                .webhook_base_url
                .map(|b| b.trim_end_matches('/').to_owned()),
            tasks: Mutex::new(Vec::new()),
        });

        // Weak: the loops must not keep the runtime alive past shutdown.
        let weak: Weak<Self> = Arc::downgrade(&rt);
        let drain = tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                let Some(rt) = weak.upgrade() else { break };
                tokio::spawn(async move { rt.run_event(ev).await });
            }
        });
        let weak = Arc::downgrade(&rt);
        let ticker = ft::spawn_ticker(host, Some(executor), cfg.tick, move |res| {
            if let Some(rt) = weak.upgrade() {
                tokio::spawn(async move { rt.finalize_resumed(res).await });
            }
        });
        rt.tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend([drain, ticker]);
        Ok(rt)
    }

    pub fn webhook_handler(self: &Arc<Self>) -> Arc<dyn hs::WebhookHandler> {
        Arc::new(Bridge { rt: self.clone() })
    }

    pub fn activator(self: &Arc<Self>) -> Arc<dyn hs::WorkflowActivator> {
        Arc::new(Activator { rt: self.clone() })
    }

    /// Agent tools provided by loaded plugins.
    #[cfg(feature = "plugins")]
    pub fn plugin_tools(&self) -> Vec<Arc<dyn hk_tools::Tool>> {
        self.plugins
            .as_ref()
            .map(|p| p.tools.clone())
            .unwrap_or_default()
    }

    pub fn host(&self) -> &Arc<LocalTriggerHost> {
        &self.host
    }

    /// Stop the background loops. In-flight runs are not awaited.
    pub fn shutdown(&self) {
        for t in self
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            t.abort();
        }
    }

    /// Boot: activate every workflow stored as active (registers webhooks, re-arms schedules
    /// from now). A workflow that fails to activate is logged and left flagged active so the
    /// operator can fix and re-activate it. Returns how many activated.
    pub async fn activate_stored(&self) -> Result<usize, ApiError> {
        let (mut ok, mut offset) = (0, 0u32);
        loop {
            let rows = self
                .api
                .store
                .list_workflows(Some(true), None, 200, offset)
                .await?;
            if rows.is_empty() {
                return Ok(ok);
            }
            offset += u32::try_from(rows.len()).unwrap_or(u32::MAX);
            for row in rows {
                let res = Workflow::from_json_value(hs::workflow_json(&row));
                match res {
                    Ok(wf) => match self.triggers.activate(&wf).await {
                        Ok(a) => {
                            ok += 1;
                            tracing::info!(workflow = %row.id, webhooks = a.webhooks.len(), schedules = a.schedules, "workflow activated");
                        }
                        Err(e) => {
                            tracing::warn!(workflow = %row.id, error = %e, "cannot activate stored workflow")
                        }
                    },
                    Err(e) => {
                        tracing::warn!(workflow = %row.id, error = %e, "stored workflow is invalid")
                    }
                }
            }
        }
    }

    /// Resume every parked execution due at `now_ms` and record the outcomes. Returns how many.
    pub async fn resume_due(&self, now_ms: i64) -> usize {
        let results = self.executor.resume_due(now_ms).await;
        let n = results.len();
        for r in results {
            self.finalize_resumed(r).await;
        }
        n
    }

    // ------------------------------------------------------------------ runs

    async fn begin(&self, workflow_id: &str, mode: &str) -> Result<Started, ApiError> {
        let row = self
            .api
            .store
            .get_workflow(workflow_id)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("workflow {workflow_id} not found")))?;
        let json = hs::workflow_json(&row);
        let wf = Workflow::from_json_value(json.clone())
            .map_err(|e| ApiError::Internal(format!("stored workflow is invalid: {e}")))?;
        let exec_id = self
            .api
            .start_execution(workflow_id, mode, Some(&json))
            .await?;
        Ok(Started { wf, exec_id })
    }

    /// Execute and record. `Err` carries the message a webhook caller may see.
    async fn run(
        &self,
        s: Started,
        node: Option<String>,
        items: Vec<Item>,
        mode: &str,
        sink: Option<Arc<dyn ResponseSink>>,
    ) -> WebhookResult {
        let Started { wf, exec_id } = s;
        let start = match node
            .or_else(|| ft::find_trigger(&wf, TriggerKind::Error).map(|n| n.name.clone()))
            .ok_or_else(|| FlowError::Invalid("workflow has no Error Trigger".into()))
        {
            Ok(n) => n,
            Err(e) => return self.fail(exec_id, &e).await,
        };
        let mut cfg = self.exec_cfg.clone();
        cfg.mode = mode.to_owned();
        if let Some(sink) = sink {
            cfg.hooks.response = Some(sink);
        }
        let mut input = ExecuteInput::new(start, items);
        input.execution_id = Some(exec_id.to_string());
        match self
            .executor
            .clone()
            .with_config(cfg)
            .execute(&wf, &input)
            .await
        {
            Ok(res) => {
                self.finalize(exec_id, &res).await;
                self.host
                    .on_execution_finished(&wf, &res, self.base_url.as_deref());
                if res.status == RunStatus::Error {
                    return Err(res.error.map(|e| e.message).unwrap_or_default());
                }
                Ok(last_items(&res))
            }
            Err(e) => self.fail(exec_id, &e).await,
        }
    }

    async fn fail(&self, exec_id: i64, e: &FlowError) -> WebhookResult {
        let msg = e.to_string();
        let data = json!({ "resultData": { "runData": {}, "error": { "message": msg } } });
        if let Err(e) = self
            .api
            .finish_execution(exec_id, "error", Some(&data))
            .await
        {
            tracing::error!(error = %e, execution = exec_id, "cannot record failed execution");
        }
        Err(msg)
    }

    /// Persist the outcome: n8n `{resultData: {runData, ...}}` envelope with credential-bearing
    /// headers blanked, then workflow static data when a node changed it.
    async fn finalize(&self, exec_id: i64, res: &ExecutionResult) {
        let status = match res.status {
            RunStatus::Success => "success",
            RunStatus::Error => "error",
            RunStatus::Waiting => "waiting",
            RunStatus::Running => "running",
        };
        let mut result = Map::new();
        match serde_json::to_value(&res.run_data) {
            Ok(v) => result.insert("runData".into(), v),
            Err(e) => {
                tracing::error!(error = %e, "cannot serialize run data");
                result.insert("runData".into(), json!({}))
            }
        };
        if let Some(n) = &res.last_node_executed {
            result.insert("lastNodeExecuted".into(), json!(n));
        }
        if let Some(e) = &res.error {
            result.insert(
                "error".into(),
                json!({ "message": e.message, "node": e.node }),
            );
        }
        let mut data = json!({ "resultData": Value::Object(result) });
        scrub_headers(&mut data);
        if let Err(e) = self
            .api
            .finish_execution(exec_id, status, Some(&data))
            .await
        {
            tracing::error!(error = %e, execution = exec_id, "cannot record execution");
        }
        if let Some(sd) = &res.static_data
            && let Ok(Some(ex)) = self.api.store.get_execution(exec_id).await
        {
            self.save_static_data(&ex.workflow_id, sd).await;
        }
    }

    async fn save_static_data(&self, workflow_id: &str, sd: &Value) {
        let store = &self.api.store;
        let Ok(Some(row)) = store.get_workflow(workflow_id).await else {
            return;
        };
        let Ok(Value::Object(mut data)) = serde_json::from_str::<Value>(&row.data) else {
            return;
        };
        data.insert("staticData".into(), sd.clone());
        if let Err(e) = store
            .update_workflow(
                &row.id,
                &row.name,
                &row.version_id,
                &Value::Object(data).to_string(),
            )
            .await
        {
            tracing::warn!(error = %e, workflow = %row.id, "cannot persist static data");
        }
    }

    /// A wait resumed by the ticker: same recording as a fresh run, minus error workflows
    /// (the snapshot's workflow is not in hand here).
    async fn finalize_resumed(&self, res: Result<ExecutionResult, FlowError>) {
        match res {
            Ok(res) => match res.execution_id.parse::<i64>() {
                Ok(id) => self.finalize(id, &res).await,
                Err(_) => {
                    tracing::warn!(execution = %res.execution_id, "resumed execution has a foreign id")
                }
            },
            Err(e) => tracing::error!(error = %e, "cannot resume waiting execution"),
        }
    }

    async fn run_event(&self, ev: TriggerEvent) {
        let mode = ev.mode.as_str();
        match self.begin(&ev.workflow_id, mode).await {
            Ok(s) => {
                let _ = self.run(s, ev.node_name, ev.items, mode, None).await;
            }
            Err(e) => {
                tracing::warn!(workflow = %ev.workflow_id, error = %e, "trigger fired for an unrunnable workflow")
            }
        }
    }
}

/// Items on the first output of the last node that ran (what `lastNode` answers with).
fn last_items(res: &ExecutionResult) -> Vec<Item> {
    res.last_node_executed
        .as_deref()
        .and_then(|n| res.run_data.get(n))
        .and_then(|runs| runs.last())
        .map_or_else(Vec::new, |r| r.output(0).to_vec())
}

// ------------------------------------------------------------------ webhooks

struct Bridge {
    rt: Arc<FlowRuntime>,
}

impl fmt::Debug for Bridge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebhookBridge")
            .field("routes", &self.rt.registry.routes().len())
            .finish()
    }
}

fn kind_of(mode: WebhookMode) -> WebhookKind {
    match mode {
        WebhookMode::Production => WebhookKind::Production,
        WebhookMode::Test => WebhookKind::Test,
    }
}

/// hk-flow webhook route -> hk-server spec. Node authentication other than `none` maps to
/// bearer-token auth (fail closed); `responseData: firstEntryBinary` and the `onReceived`
/// response text are not carried over by hk-server's responder yet.
fn server_spec(r: &ft::WebhookRoute) -> hs::WebhookSpec {
    let mut s = hs::WebhookSpec::new(&r.workflow_id, &r.spec.route);
    s.methods = r.spec.methods.clone();
    s.response_mode = match r.spec.response_mode {
        ft::ResponseMode::OnReceived => hs::ResponseMode::OnReceived,
        ft::ResponseMode::LastNode => hs::ResponseMode::LastNode,
        ft::ResponseMode::ResponseNode => hs::ResponseMode::ResponseNode,
    };
    s.response_data = match r.spec.response_data.as_str() {
        "allEntries" => hs::LastNodeData::AllEntries,
        "noData" => hs::LastNodeData::NoData,
        _ => hs::LastNodeData::FirstEntryJson,
    };
    s.response_code = r.spec.response_code;
    s.auth = if r.spec.authentication == "none" {
        hs::WebhookAuth::None
    } else {
        hs::WebhookAuth::Bearer
    };
    s
}

#[async_trait]
impl hs::WebhookHandler for Bridge {
    fn lookup(&self, mode: WebhookMode, method: &str, path: &str) -> Option<hs::WebhookRoute> {
        let m = self.rt.registry.resolve(kind_of(mode), method, path)?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (
            &m.route.workflow_id,
            &m.route.node_name,
            &m.route.spec.route,
            mode == WebhookMode::Test,
        )
            .hash(&mut h);
        Some(hs::WebhookRoute {
            id: h.finish(),
            spec: Arc::new(server_spec(&m.route)),
            params: m.params,
        })
    }

    fn unregister_owner(&self, owner: &str) {
        self.rt
            .registry
            .unregister_workflow(owner, WebhookKind::Production);
        self.rt
            .registry
            .unregister_workflow(owner, WebhookKind::Test);
    }

    async fn handle(
        &self,
        _route: &hs::WebhookRoute,
        call: WebhookCall,
    ) -> Result<WebhookRun, ApiError> {
        let rt = &self.rt;
        let kind = kind_of(call.mode);
        // Re-resolve: the route id is only a lookup token, the registry is the truth.
        let m = rt
            .registry
            .resolve(kind, &call.method, &call.path)
            .ok_or_else(|| ApiError::NotFound("webhook no longer registered".into()))?;
        let (wf_id, node) = (m.route.workflow_id.clone(), m.route.node_name.clone());
        let mode = match kind {
            WebhookKind::Production => "webhook",
            WebhookKind::Test => "manual",
        };
        let started = rt.begin(&wf_id, mode).await?;
        let exec_id = started.exec_id;

        let mut item = call.item;
        if let Some(base) = &rt.base_url {
            item.json.insert(
                "webhookUrl".into(),
                json!(format!("{base}/{}", m.route.url_path())),
            );
        }
        item.json
            .insert("executionMode".into(), json!(kind.execution_mode()));

        let (tx, done) = oneshot::channel();
        let rt2 = rt.clone();
        let sink = call.sink;
        tokio::spawn(async move {
            let out = rt2
                .run(started, Some(node), vec![item], mode, Some(sink))
                .await;
            let _ = tx.send(out);
        });
        if kind == WebhookKind::Test {
            // test listeners are one-shot
            rt.registry.unregister_workflow(&wf_id, WebhookKind::Test);
        }
        Ok(WebhookRun {
            execution_id: Some(exec_id),
            done,
        })
    }
}

// ----------------------------------------------------------------- activation

struct Activator {
    rt: Arc<FlowRuntime>,
}

impl fmt::Debug for Activator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Activator")
    }
}

#[async_trait]
impl hs::WorkflowActivator for Activator {
    async fn activate(&self, workflow: &Value) -> Result<(), String> {
        let wf = Workflow::from_json_value(workflow.clone()).map_err(|e| e.to_string())?;
        self.rt
            .triggers
            .activate(&wf)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn deactivate(&self, workflow_id: &str) -> Result<(), String> {
        self.rt
            .triggers
            .deactivate(workflow_id)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Poll cursors live in the workflow's `staticData`, like n8n's `getWorkflowStaticData`.
#[cfg(feature = "integrations")]
#[derive(Debug)]
struct StoreStatic {
    store: Store,
}

#[cfg(feature = "integrations")]
#[async_trait]
impl hk_integrations::poll::StaticDataStore for StoreStatic {
    async fn load_static(&self, workflow_id: &str) -> Result<Option<Value>, FlowError> {
        let row = self
            .store
            .get_workflow(workflow_id)
            .await
            .map_err(|e| FlowError::Store(e.to_string()))?;
        Ok(row
            .and_then(|r| serde_json::from_str::<Value>(&r.data).ok())
            .and_then(|v| v.get("staticData").cloned()))
    }

    async fn save_static(&self, workflow_id: &str, data: Value) -> Result<(), FlowError> {
        let Some(row) = self
            .store
            .get_workflow(workflow_id)
            .await
            .map_err(|e| FlowError::Store(e.to_string()))?
        else {
            return Ok(());
        };
        let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(&row.data) else {
            return Ok(());
        };
        obj.insert("staticData".into(), data);
        self.store
            .update_workflow(
                &row.id,
                &row.name,
                &row.version_id,
                &Value::Object(obj).to_string(),
            )
            .await
            .map_err(|e| FlowError::Store(e.to_string()))?;
        Ok(())
    }
}

// ----------------------------------------------------------------- credentials

/// Resolves a node's credential reference from the stored credential: type must match the
/// metadata row, values come from the secret store (`credential.<id>` JSON blob). Errors
/// name the credential, never its values.
struct StoredCredentials {
    store: Store,
    secrets: Arc<dyn SecretStore>,
}

impl fmt::Debug for StoredCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoredCredentials")
    }
}

#[async_trait]
impl CredentialResolver for StoredCredentials {
    async fn resolve(
        &self,
        credential_type: &str,
        reference: &CredentialRef,
    ) -> Result<Map<String, Value>, FlowError> {
        let id = reference.id.as_deref().ok_or_else(|| {
            FlowError::Invalid(format!("credential of type {credential_type} has no id"))
        })?;
        let row = self
            .store
            .get_credential(id)
            .await
            .map_err(|_| FlowError::Store("credential lookup failed".into()))?;
        match row {
            Some(r) if r.kind == credential_type => {}
            Some(_) => {
                return Err(FlowError::Invalid(format!(
                    "credential {id} is not of type {credential_type}"
                )));
            }
            None => return Err(FlowError::Invalid(format!("credential {id} not found"))),
        }
        let blob = self
            .store
            .get_secret(self.secrets.as_ref(), &format!("credential.{id}"))
            .map_err(|_| FlowError::Store("credential secret unavailable".into()))?
            .ok_or_else(|| FlowError::Invalid(format!("credential {id} has no stored value")))?;
        match serde_json::from_str::<Value>(blob.expose()) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(FlowError::Invalid(format!("credential {id} is corrupt"))),
        }
    }

    async fn persist(
        &self,
        credential_type: &str,
        reference: &CredentialRef,
        patch: Map<String, Value>,
    ) -> Result<(), FlowError> {
        let mut data = self.resolve(credential_type, reference).await?;
        data.extend(patch);
        let id = reference.id.as_deref().unwrap_or_default();
        self.store
            .put_secret(
                self.secrets.as_ref(),
                &format!("credential.{id}"),
                hk_store::Secret::new(Value::Object(data).to_string()),
            )
            .await
            .map_err(|_| FlowError::Store("credential could not be updated".into()))
    }
}

// ------------------------------------------------------------------ agent tools

#[async_trait]
impl hs::FlowToolHost for FlowRuntime {
    fn nodes(&self) -> &NodeRegistry {
        &self.nodes
    }

    /// Manual-mode run for `flow_run`: pin data honoured, no activation, no error workflow.
    /// Stored workflows get an execution row so the run shows up in the UI history.
    async fn run_test(&self, req: hs::TestRun) -> Result<ExecutionResult, String> {
        let exec_id = match &req.workflow_id {
            Some(id) => {
                let json = req.workflow.to_json_value().map_err(|e| e.to_string())?;
                Some(
                    self.api
                        .start_execution(id, "manual", Some(&json))
                        .await
                        .map_err(|e| e.to_string())?,
                )
            }
            None => None,
        };
        let mut cfg = self.exec_cfg.clone();
        cfg.mode = "manual".into();
        cfg.use_pin_data = true;
        let mut input = ExecuteInput::new(req.start_node, req.items);
        input.execution_id = exec_id.map(|i| i.to_string());
        match self
            .executor
            .clone()
            .with_config(cfg)
            .execute(&req.workflow, &input)
            .await
        {
            Ok(res) => {
                if let Some(id) = exec_id {
                    self.finalize(id, &res).await;
                }
                Ok(res)
            }
            Err(e) => {
                if let Some(id) = exec_id {
                    let _ = self.fail(id, &e).await;
                }
                Err(e.to_string())
            }
        }
    }

    fn scrub(&self, data: &mut Value) {
        scrub_headers(data);
    }
}
