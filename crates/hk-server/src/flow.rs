//! Shared state for the n8n-shaped surface (webhooks, REST v1 workflows / executions /
//! credentials, execution events).

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use hk_store::{SecretStore, Store};
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

use crate::error::ApiError;
use crate::flow_tools::FlowToolHost;
use crate::runtime::ServerEvent;
use crate::webhook::{WebhookHandler, WebhookRegistry};

impl From<hk_store::StoreError> for ApiError {
    fn from(e: hk_store::StoreError) -> Self {
        // sqlx messages can echo SQL/paths; keep the wire message generic, log the detail.
        tracing::error!(error = %e, "store error");
        Self::Internal("storage error".into())
    }
}

/// Seam to the trigger manager (hk-flow P4.4): turn a stored workflow's triggers on/off.
/// Errors surface to the caller as 400 (e.g. "no trigger node"). Without one, activate /
/// deactivate only flip the stored flag.
#[async_trait]
pub trait WorkflowActivator: Send + Sync + fmt::Debug {
    /// `workflow` is the n8n-shaped JSON the API returns (id, name, nodes, ...).
    async fn activate(&self, workflow: &Value) -> Result<(), String>;
    async fn deactivate(&self, workflow_id: &str) -> Result<(), String>;
}

/// Credential type schemas (`GET /credentials/schema/{type}`) and validation source.
pub trait CredentialSchemas: Send + Sync + fmt::Debug {
    /// JSON schema (`type: object`, `properties`, `required`) or `None` for unknown types.
    fn schema(&self, type_name: &str) -> Option<Value>;
}

/// A few core credential types. Integration crates supply their own provider that wraps this.
#[derive(Debug, Default, Clone, Copy)]
pub struct BuiltinCredentialSchemas;

impl CredentialSchemas for BuiltinCredentialSchemas {
    fn schema(&self, type_name: &str) -> Option<Value> {
        let (props, required): (&[&str], &[&str]) = match type_name {
            "httpBasicAuth" => (&["user", "password"], &["user", "password"]),
            "httpHeaderAuth" | "httpQueryAuth" => (&["name", "value"], &["name", "value"]),
            "httpBearerAuth" => (&["token"], &["token"]),
            "httpCustomAuth" => (&["json"], &["json"]),
            _ => return None,
        };
        let properties: Map<String, Value> = props
            .iter()
            .map(|p| ((*p).to_owned(), json!({ "type": "string" })))
            .collect();
        Some(json!({
            "additionalProperties": false,
            "type": "object",
            "properties": properties,
            "required": required,
        }))
    }
}

/// Everything the n8n-shaped routes need. Cheap to clone.
#[derive(Clone)]
pub struct FlowApi {
    pub store: Store,
    /// Credential values and webhook HMAC keys live here, never in `store`.
    pub secrets: Arc<dyn SecretStore>,
    pub webhooks: Arc<dyn WebhookHandler>,
    pub activator: Option<Arc<dyn WorkflowActivator>>,
    /// Backs `POST /workflows/{id}/run` (manual-mode test runs). Without one the route answers 400.
    pub test_runner: Option<Arc<dyn FlowToolHost>>,
    pub credential_schemas: Arc<dyn CredentialSchemas>,
    events: broadcast::Sender<ServerEvent>,
    /// Executions mirrored as jobs (progress per node, cancel). `None` = not tracked.
    jobs: Option<hk_core::JobRegistry>,
    live_jobs: Arc<std::sync::Mutex<std::collections::HashMap<i64, hk_core::JobHandle>>>,
}

impl fmt::Debug for FlowApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlowApi")
            .field("webhooks", &self.webhooks)
            .finish_non_exhaustive()
    }
}

impl FlowApi {
    /// Defaults: in-server [`WebhookRegistry`], no activator, builtin credential schemas.
    pub fn new(store: Store, secrets: Arc<dyn SecretStore>) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            store,
            secrets,
            webhooks: Arc::new(WebhookRegistry::new()),
            activator: None,
            test_runner: None,
            credential_schemas: Arc::new(BuiltinCredentialSchemas),
            events,
            jobs: None,
            live_jobs: Arc::default(),
        }
    }

    /// Track executions in `jobs` (shared with the agent runtime for one Activity view).
    #[must_use]
    pub fn with_jobs(mut self, jobs: Option<hk_core::JobRegistry>) -> Self {
        self.jobs = jobs;
        self
    }

    /// Cancel token + per-node progress hook for a started execution; `None` when untracked.
    pub fn execution_control(&self, id: i64) -> Option<hk_flow::RunControl> {
        let live = self.live_jobs.clone();
        let cancel = live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)?
            .token()
            .clone();
        Some(hk_flow::RunControl {
            cancel,
            on_node: Some(Arc::new(move |node: &str, runs: usize, total: usize| {
                if let Some(h) = live
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&id)
                {
                    h.phase(node);
                    // Loops can run nodes more than once; never claim completion early.
                    let f = (runs as f32 / total.max(1) as f32).min(0.95);
                    h.progress(hk_core::Progress::Determinate(f));
                }
            })),
        })
    }

    #[must_use]
    pub fn with_webhooks(mut self, w: Arc<dyn WebhookHandler>) -> Self {
        self.webhooks = w;
        self
    }

    #[must_use]
    pub fn with_activator(mut self, a: Arc<dyn WorkflowActivator>) -> Self {
        self.activator = Some(a);
        self
    }

    #[must_use]
    pub fn with_test_runner(mut self, r: Arc<dyn FlowToolHost>) -> Self {
        self.test_runner = Some(r);
        self
    }

    #[must_use]
    pub fn with_credential_schemas(mut self, s: Arc<dyn CredentialSchemas>) -> Self {
        self.credential_schemas = s;
        self
    }

    /// Execution event feed for WS clients.
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.events.subscribe()
    }

    /// Record a new execution row and announce it (`execution/started`).
    pub async fn start_execution(
        &self,
        workflow_id: &str,
        mode: &str,
        workflow_snapshot: Option<&Value>,
    ) -> Result<i64, ApiError> {
        let snap = workflow_snapshot.map(Value::to_string);
        let row = self
            .store
            .insert_execution(workflow_id, mode, "running", snap.as_deref())
            .await?;
        if let Some(reg) = &self.jobs {
            let h = reg.start(
                hk_core::JobSpec::new(
                    hk_core::JobKind::FlowExecution,
                    format!("{workflow_id} #{}", row.id),
                )
                .key(format!("exec:{}", row.id))
                .meta(json!({ "workflow_id": workflow_id, "execution_id": row.id, "mode": mode })),
                None,
            );
            self.live_jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(row.id, h);
        }
        self.emit(
            "execution/started",
            row.id,
            json!({ "workflow_id": workflow_id, "mode": mode, "started_at": crate::n8n::iso(row.started_ms) }),
        );
        Ok(row.id)
    }

    /// Persist the outcome and announce it (`execution/finished`). `data` is the n8n
    /// `{resultData: {runData}}` envelope; the engine guarantees it carries no secrets.
    pub async fn finish_execution(
        &self,
        id: i64,
        status: &str,
        data: Option<&Value>,
    ) -> Result<(), ApiError> {
        let job = self
            .live_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        if let Some(h) = job {
            match status {
                "success" => h.finish(),
                "canceled" => h.cancelled(),
                // Parked in a Wait: the job ends; the resumed run is not tracked as a job yet.
                "waiting" => {
                    h.log("waiting");
                    h.finish();
                }
                other => h.fail(other),
            }
        }
        let text = data.map(Value::to_string);
        let finished = status == "success";
        if !self
            .store
            .finish_execution(id, status, finished, text.as_deref())
            .await?
        {
            return Err(ApiError::NotFound(format!("execution {id} not found")));
        }
        let wf = self
            .store
            .get_execution(id)
            .await?
            .map(|e| e.workflow_id)
            .unwrap_or_default();
        self.emit(
            "execution/finished",
            id,
            json!({ "workflow_id": wf, "status": status, "finished": finished }),
        );
        Ok(())
    }

    fn emit(&self, name: &str, execution_id: i64, data: Value) {
        // No receivers is normal.
        let _ = self.events.send(ServerEvent {
            name: name.into(),
            session_id: execution_id.to_string(),
            data,
        });
    }
}
