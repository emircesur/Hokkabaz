//! Test runs through `LocalCore`: event stream, pin data, persistence. No network.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use hk_ffi::dto::{CoreProfile, FlowRunEventDto, FlowRunEventKind as K, FlowRunModeDto};
use hk_ffi::error::HkErrorKind;
use hk_ffi::handle::CoreHandle;
use hk_ffi::local::{CoreOptions, LocalCore};
use hk_store::MemorySecretStore;
use serde_json::{Value, json};

fn workflow() -> String {
    json!({
        "id": "wf-1",
        "name": "Set flow",
        "nodes": [
            {"id": "1", "name": "Trigger", "type": "n8n-nodes-base.manualTrigger", "typeVersion": 1, "position": [0, 0], "parameters": {}},
            {"id": "2", "name": "Set", "type": "n8n-nodes-base.set", "typeVersion": 3.4, "position": [220, 0],
             "parameters": {"mode": "raw", "jsonOutput": "{\"a\": 1}", "includeOtherFields": false, "options": {}}},
            {"id": "3", "name": "After", "type": "n8n-nodes-base.noOp", "typeVersion": 1, "position": [440, 0], "parameters": {}}
        ],
        "connections": {
            "Trigger": {"main": [[{"node": "Set", "type": "main", "index": 0}]]},
            "Set": {"main": [[{"node": "After", "type": "main", "index": 0}]]}
        },
        "settings": {"executionOrder": "v1"}
    })
    .to_string()
}

async fn open(dir: &tempfile::TempDir) -> Arc<LocalCore> {
    LocalCore::open(
        dir.path().join("data"),
        CoreProfile::Mobile,
        CoreOptions { secrets: Some(Arc::new(MemorySecretStore::default())), extra_providers: Vec::new(), refresh_catalog: false },
    )
    .await
    .unwrap()
}

async fn collect(core: &LocalCore, id: &str) -> Vec<FlowRunEventDto> {
    let feed = core.run_events(id).unwrap();
    tokio::time::timeout(Duration::from_secs(20), feed.collect::<Vec<_>>()).await.expect("run did not finish")
}

fn output(ev: &FlowRunEventDto) -> Value {
    serde_json::from_str(ev.output_json.as_deref().unwrap()).unwrap()
}

#[tokio::test]
async fn set_workflow_streams_node_events_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let core = open(&dir).await;
    let id = core.run_workflow(workflow(), FlowRunModeDto::Test, None, None, None).await.unwrap();

    let evs = collect(&core, &id).await;
    let kinds: Vec<K> = evs.iter().map(|e| e.kind).collect();
    assert_eq!(kinds.first(), Some(&K::RunStarted));
    assert_eq!(kinds.last(), Some(&K::RunFinished));
    assert_eq!(kinds.iter().filter(|k| **k == K::NodeStarted).count(), kinds.iter().filter(|k| **k == K::NodeFinished).count());

    let finished: Vec<&FlowRunEventDto> = evs.iter().filter(|e| e.kind == K::NodeFinished).collect();
    let names: Vec<&str> = finished.iter().filter_map(|e| e.node.as_deref()).collect();
    assert_eq!(names, ["Trigger", "Set", "After"], "nodes arrive in execution order, the trigger first");
    let set = finished[1];
    assert_eq!(set.item_count, 1);
    assert_eq!(set.status.as_deref(), Some("success"));
    assert_eq!(output(set)[0][0]["json"]["a"], 1);

    // A subscriber that attaches after the end replays the same history.
    assert_eq!(collect(&core, &id).await, evs);

    let runs = core.list_runs(Some("wf-1".into()), 10).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].id.as_str(), runs[0].status.as_str(), runs[0].mode.as_str()), (id.as_str(), "success", "test"));
    assert_eq!(runs[0].workflow_name, "Set flow");
    assert!(core.list_runs(Some("other".into()), 10).await.unwrap().is_empty());

    let d = core.get_run(&id).await.unwrap();
    let rd: Value = serde_json::from_str(&d.run_data_json).unwrap();
    assert_eq!(rd["Set"][0]["itemCounts"], json!([1]));
    assert!(d.workflow_json.is_some());

    // Survives a restart of the core (files, not memory).
    core.shutdown().await;
    let again = open(&dir).await;
    assert_eq!(again.list_runs(None, 10).await.unwrap().len(), 1);
    assert_eq!(collect(&again, &id).await.last().map(|e| e.kind), Some(K::RunFinished));
}

#[tokio::test]
async fn pin_data_is_honoured_in_test_mode_and_ignored_in_manual() {
    let dir = tempfile::tempdir().unwrap();
    let core = open(&dir).await;
    let pin = json!({"Set": [{"json": {"pinned": true}}]}).to_string();

    let id = core.run_workflow(workflow(), FlowRunModeDto::Test, None, None, Some(pin.clone())).await.unwrap();
    let evs = collect(&core, &id).await;
    let set = evs.iter().find(|e| e.kind == K::NodeFinished && e.node.as_deref() == Some("Set")).unwrap();
    assert_eq!(output(set)[0][0]["json"], json!({"pinned": true}));

    let id = core.run_workflow(workflow(), FlowRunModeDto::Manual, None, None, Some(pin)).await.unwrap();
    let evs = collect(&core, &id).await;
    let set = evs.iter().find(|e| e.kind == K::NodeFinished && e.node.as_deref() == Some("Set")).unwrap();
    assert_eq!(output(set)[0][0]["json"], json!({"a": 1}));
}

#[tokio::test]
async fn failing_node_ends_the_run_with_an_error_event() {
    let dir = tempfile::tempdir().unwrap();
    let core = open(&dir).await;
    let mut wf: Value = serde_json::from_str(&workflow()).unwrap();
    wf["nodes"][1]["parameters"]["jsonOutput"] = json!("{not json");
    let id = core.run_workflow(wf.to_string(), FlowRunModeDto::Manual, None, None, None).await.unwrap();
    let evs = collect(&core, &id).await;
    let last = evs.last().unwrap();
    assert_eq!(last.kind, K::RunError);
    assert!(last.error.is_some());
    assert_eq!(core.list_runs(None, 5).await.unwrap()[0].status, "error");
}

#[tokio::test]
async fn bad_input_fails_before_a_run_exists() {
    let dir = tempfile::tempdir().unwrap();
    let core = open(&dir).await;
    let e = core.run_workflow("{".into(), FlowRunModeDto::Test, None, None, None).await.unwrap_err();
    assert_eq!(e.kind, HkErrorKind::InvalidArgument);
    let e = core.run_workflow(workflow(), FlowRunModeDto::Test, Some("Nope".into()), None, None).await.unwrap_err();
    assert_eq!(e.kind, HkErrorKind::InvalidArgument);
    let e = core.run_workflow(workflow(), FlowRunModeDto::Test, None, Some("{}".into()), None).await.unwrap_err();
    assert_eq!(e.kind, HkErrorKind::InvalidArgument);
    assert_eq!(core.run_events("run-0-0").err().map(|e| e.kind), Some(HkErrorKind::NotFound));
    assert_eq!(core.get_run("../hk.db").await.unwrap_err().kind, HkErrorKind::NotFound);
    assert_eq!(core.set_workflow_active(workflow(), true).await.unwrap_err().kind, HkErrorKind::Unsupported);
}
