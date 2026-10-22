//! Flow test runs: start a run, follow its events, browse past runs.
//!
//! Workflows are the canvas's n8n JSON, passed whole; the active core decides where it runs
//! (in-process, or on the connected instance).

use crate::dto::{FlowRunDetailDto, FlowRunEventDto, FlowRunModeDto, FlowRunSummaryDto};
use crate::error::{HkError, HkErrorKind};
use crate::frb_generated::StreamSink;
use crate::handle::core;
use futures::StreamExt;

/// Start a run and return its id. `trigger_node` `None` = the Manual Trigger (or the first
/// trigger); `input_items_json` = JSON array of objects the trigger emits (default one empty
/// item); `pin_data_json` = `{node: [items]}` replacing the workflow's `pinData` for this run.
/// `Test` honours pins, `Manual` ignores them. Follow with `run_events`.
pub async fn run_workflow(
    workflow_json: String,
    mode: FlowRunModeDto,
    trigger_node: Option<String>,
    input_items_json: Option<String>,
    pin_data_json: Option<String>,
) -> Result<String, HkError> {
    core()?.run_workflow(workflow_json, mode, trigger_node, input_items_json, pin_data_json).await
}

/// Events of one run from its start until `RunFinished` / `RunError`. Safe to attach after
/// `run_workflow` returned (the history replays).
pub async fn run_events(run_id: String, sink: StreamSink<FlowRunEventDto>) -> Result<(), HkError> {
    let mut feed = core()?.run_events(&run_id)?;
    while let Some(ev) = feed.next().await {
        sink.add(ev).map_err(|e| HkError::new(HkErrorKind::Closed, e.to_string()))?;
    }
    Ok(())
}

/// Past runs, newest first. `workflow_id` `None` = all workflows.
pub async fn list_runs(workflow_id: Option<String>, limit: u32) -> Result<Vec<FlowRunSummaryDto>, HkError> {
    core()?.list_runs(workflow_id, limit).await
}

/// One run with its (size-bounded) per-node data and the workflow it ran.
pub async fn get_run(run_id: String) -> Result<FlowRunDetailDto, HkError> {
    core()?.get_run(&run_id).await
}

/// Arm (`true`) or disarm the workflow's triggers on the connected instance. `Unsupported` on
/// the in-process core. Returns the resulting active flag.
pub async fn set_workflow_active(workflow_json: String, active: bool) -> Result<bool, HkError> {
    core()?.set_workflow_active(workflow_json, active).await
}
