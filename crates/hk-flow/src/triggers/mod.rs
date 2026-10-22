//! Triggers and durable waits.
//!
//! - [`TriggerSpec`] / [`trigger_spec`]: what a trigger node asks the runtime to arm,
//!   parsed from its (static) parameters.
//! - [`schedule`]: cron lowering + next-fire computation (timezone and DST aware).
//! - [`webhook`]: [`WebhookRegistry`] that hk-server mounts, request -> item parsing.
//! - [`events`]: native app-event and file-watch triggers.
//! - [`host`]: [`TriggerHost`] (activate/deactivate) and [`LocalTriggerHost`], which arms
//!   everything and emits [`TriggerEvent`]s into a [`TriggerSink`]; the daemon/server owns
//!   the runtime that turns an event into an `Executor` run.
//! - [`wait`]: [`WaitStore`] seam for durable Wait nodes (executor side in `executor.rs`).
//!
//! Item builders for the form, chat, manual and error triggers live here; those triggers
//! have no runtime of their own in this crate (the server hosts form/chat pages and
//! hands submissions over as items).

pub mod events;
pub mod host;
pub mod schedule;
pub mod wait;
pub mod webhook;

use serde_json::{Map, Value, json};

use crate::error::FlowError;
use crate::executor::ExecutionResult;
use crate::item::Item;
use crate::nodes::date::{from_millis, to_iso};
use crate::run_data::RunStatus;
use crate::workflow::{Node, Workflow};

pub use events::{
    APP_EVENT_TYPE, AppEvent, AppEventBus, AppEventSpec, FILE_WATCH_TYPE, FileWatchSpec,
};
pub use host::{
    Activation, ChannelSink, FnSink, LocalTriggerHost, TriggerEvent, TriggerHost, TriggerMode,
    TriggerSink, spawn_ticker,
};
pub use schedule::{SCHEDULE_TYPE, ScheduleRule, ScheduleSpec, schedule_item};
pub use webhook::{
    PreparedWebhook, ResponseMode, WEBHOOK_TYPE, WebhookError, WebhookKind, WebhookMatch,
    WebhookRegistry, WebhookRequest, WebhookRoute, WebhookSpec, last_node_response,
    parse_waiting_path, request_to_item,
};

pub const MANUAL_TYPE: &str = "n8n-nodes-base.manualTrigger";
/// Pre-manualTrigger entry node of old exports.
pub const LEGACY_START_TYPE: &str = "n8n-nodes-base.start";
pub const FORM_TYPE: &str = "n8n-nodes-base.formTrigger";
pub const CHAT_TYPE: &str = "@n8n/n8n-nodes-langchain.chatTrigger";
pub const EXECUTE_WORKFLOW_TRIGGER_TYPE: &str = "n8n-nodes-base.executeWorkflowTrigger";
pub const ERROR_TRIGGER_TYPE: &str = "n8n-nodes-base.errorTrigger";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerKind {
    Manual,
    Schedule,
    Webhook,
    Form,
    Chat,
    ExecuteWorkflow,
    Error,
    AppEvent,
    FileWatch,
}

impl TriggerKind {
    pub fn of_type(node_type: &str) -> Option<Self> {
        Some(match node_type {
            MANUAL_TYPE | LEGACY_START_TYPE => Self::Manual,
            SCHEDULE_TYPE => Self::Schedule,
            WEBHOOK_TYPE => Self::Webhook,
            FORM_TYPE => Self::Form,
            CHAT_TYPE => Self::Chat,
            EXECUTE_WORKFLOW_TRIGGER_TYPE => Self::ExecuteWorkflow,
            ERROR_TRIGGER_TYPE => Self::Error,
            APP_EVENT_TYPE => Self::AppEvent,
            FILE_WATCH_TYPE => Self::FileWatch,
            _ => return None,
        })
    }
}

/// First enabled trigger node of `kind` (start node for error workflows, sub-workflow
/// calls, manual runs).
pub fn find_trigger(wf: &Workflow, kind: TriggerKind) -> Option<&Node> {
    wf.nodes
        .iter()
        .find(|n| !n.is_disabled() && TriggerKind::of_type(&n.node_type) == Some(kind))
}

/// What the runtime must arm for one trigger node.
#[derive(Debug, Clone, PartialEq)]
pub enum TriggerSpec {
    Manual,
    Schedule(ScheduleSpec),
    Webhook(WebhookSpec),
    /// Form and chat triggers: item shape only, the server hosts the page/endpoint.
    Form,
    Chat,
    ExecuteWorkflow,
    Error,
    AppEvent(AppEventSpec),
    FileWatch(FileWatchSpec),
}

/// `Ok(None)` = not a trigger node. Parameters are read statically (n8n resolves trigger
/// parameters at activation, with no item in scope).
pub fn trigger_spec(node: &Node) -> Result<Option<TriggerSpec>, FlowError> {
    Ok(Some(match TriggerKind::of_type(&node.node_type) {
        None => return Ok(None),
        Some(TriggerKind::Manual) => TriggerSpec::Manual,
        Some(TriggerKind::Schedule) => TriggerSpec::Schedule(ScheduleSpec::from_node(node)?),
        Some(TriggerKind::Webhook) => TriggerSpec::Webhook(WebhookSpec::from_node(node)?),
        Some(TriggerKind::Form) => TriggerSpec::Form,
        Some(TriggerKind::Chat) => TriggerSpec::Chat,
        Some(TriggerKind::ExecuteWorkflow) => TriggerSpec::ExecuteWorkflow,
        Some(TriggerKind::Error) => TriggerSpec::Error,
        Some(TriggerKind::AppEvent) => TriggerSpec::AppEvent(AppEventSpec::from_node(node)?),
        Some(TriggerKind::FileWatch) => TriggerSpec::FileWatch(FileWatchSpec::from_node(node)?),
    }))
}

// ------------------------------------------------------------------ item shapes

pub(crate) fn iso_at(ms: i64) -> String {
    from_millis(ms, chrono_tz::Tz::UTC).map_or_else(String::new, |d| to_iso(&d))
}

/// Manual Trigger start items: n8n emits one empty item.
pub fn manual_items() -> Vec<Item> {
    vec![Item::default()]
}

/// Form Trigger: submitted fields by label, then `submittedAt` and `formMode`.
pub fn form_item(fields: Map<String, Value>, test: bool, submitted_at_ms: i64) -> Item {
    let mut m = fields;
    m.insert("submittedAt".into(), json!(iso_at(submitted_at_ms)));
    m.insert(
        "formMode".into(),
        json!(if test { "test" } else { "production" }),
    );
    Item::new(m)
}

/// Chat Trigger: `{ sessionId, action: "sendMessage", chatInput }`.
pub fn chat_item(session_id: &str, chat_input: &str) -> Item {
    Item::from_json(json!({
        "sessionId": session_id,
        "action": "sendMessage",
        "chatInput": chat_input,
    }))
}

/// Error Trigger item for a failed execution of `wf` (n8n docs shape, minus `stack`).
/// Only the error message is carried over, never node parameters or credentials.
pub fn error_trigger_item(
    wf: &Workflow,
    res: &ExecutionResult,
    mode: &str,
    base_url: Option<&str>,
) -> Item {
    let mut exec = Map::new();
    exec.insert("id".into(), json!(res.execution_id));
    if let Some(b) = base_url {
        exec.insert(
            "url".into(),
            json!(format!(
                "{}/workflow/{}/executions/{}",
                b.trim_end_matches('/'),
                wf.id.as_deref().unwrap_or(""),
                res.execution_id
            )),
        );
    }
    let message = res.error.as_ref().map_or("", |e| e.message.as_str());
    exec.insert("error".into(), json!({"message": message}));
    if let Some(n) = res
        .error
        .as_ref()
        .and_then(|e| e.node.as_ref())
        .or(res.last_node_executed.as_ref())
    {
        exec.insert("lastNodeExecuted".into(), json!(n));
    }
    exec.insert("mode".into(), json!(mode));
    Item::from_json(json!({
        "execution": Value::Object(exec),
        "workflow": {"id": wf.id, "name": wf.name},
    }))
}

/// Failed (not merely finished) execution.
pub fn is_failure(res: &ExecutionResult) -> bool {
    res.status == RunStatus::Error
}
