//! Durable waits: suspend, snapshot round-trip, resume after a "restart" (new executor,
//! same store), webhook resume and timers.

use std::sync::Arc;

use hk_flow::{
    ExecuteInput, ExecutionResult, Executor, ExecutorConfig, FlowError, InMemoryRunStore,
    InMemoryWaitStore, Item, NodeRegistry, ResumeReason, RunStatus, WaitRecord, WaitStore,
    WaitTrigger, Workflow, register_core_nodes,
};
use serde_json::{Value, json};

fn registry() -> Arc<NodeRegistry> {
    let mut r = NodeRegistry::new();
    register_core_nodes(&mut r).unwrap();
    Arc::new(r)
}

fn node(name: &str, ty: &str, ver: f64, x: i64, params: Value) -> Value {
    json!({"name": name, "type": format!("n8n-nodes-base.{ty}"), "typeVersion": ver,
           "position": [x, 0], "parameters": params})
}

fn mark(name: &str, x: i64, k: &str) -> Value {
    node(
        name,
        "set",
        3.4,
        x,
        json!({"mode": "raw", "jsonOutput": format!("{{\"{k}\": true}}"), "includeOtherFields": true, "options": {}}),
    )
}

/// Trigger -> Before -> [Wait -> After, Side]. v0 order runs Wait first, so Side is still
/// queued when Wait suspends and must come back with the snapshot.
fn wf(wait_params: Value) -> Workflow {
    Workflow::from_json_value(json!({
        "id": "wf-1", "name": "waiter",
        "nodes": [
            node("Trigger", "manualTrigger", 1.0, 0, json!({})),
            mark("Before", 200, "before"),
            node("Wait", "wait", 1.1, 400, wait_params),
            mark("After", 600, "after"),
            mark("Side", 200, "side"),
        ],
        "connections": {
            "Trigger": {"main": [[{"node": "Before", "type": "main", "index": 0}]]},
            "Before": {"main": [[
                {"node": "Wait", "type": "main", "index": 0},
                {"node": "Side", "type": "main", "index": 0}]]},
            "Wait": {"main": [[{"node": "After", "type": "main", "index": 0}]]},
        },
        "settings": {"executionOrder": "v0"}
    }))
    .unwrap()
}

fn start() -> ExecuteInput {
    let mut i = ExecuteInput::new("Trigger", vec![Item::from_json(json!({"k": 1}))]);
    i.execution_id = Some("e-1".into());
    i
}

fn out(res: &ExecutionResult, node: &str) -> Vec<Value> {
    res.run_data[node][0]
        .output(0)
        .iter()
        .map(|i| Value::Object(i.json.clone()))
        .collect()
}

fn executor(store: &Arc<InMemoryWaitStore>) -> Executor {
    Executor::new(registry())
        .with_wait_store(store.clone())
        .with_config(ExecutorConfig {
            frozen_now_ms: Some(1_000_000),
            webhook_base_url: Some("https://hk.example".into()),
            ..ExecutorConfig::default()
        })
}

#[tokio::test(flavor = "current_thread")]
async fn timer_wait_suspends_and_resumes_after_restart() {
    let store = Arc::new(InMemoryWaitStore::new());
    let w = wf(json!({"resume": "timeInterval", "amount": 2, "unit": "hours"}));

    let first = executor(&store).execute(&w, &start()).await.unwrap();
    assert_eq!(first.status, RunStatus::Waiting);
    assert!(first.error.is_none());
    // ran up to the Wait; the side branch was queued behind it and is parked, not lost
    assert!(first.run_data.contains_key("Before"));
    assert!(!first.run_data.contains_key("Wait"));
    assert!(!first.run_data.contains_key("After"));
    assert!(!first.run_data.contains_key("Side"));
    let rec = first.wait.clone().unwrap();
    assert_eq!(rec.request.on, WaitTrigger::Time);
    assert_eq!(rec.request.resume_at, Some(1_000_000 + 2 * 3_600_000));

    // snapshot survives JSON (what a SQLite store would persist)
    let text = rec.to_json().unwrap();
    assert_eq!(WaitRecord::from_json(&text).unwrap(), rec);
    assert!(!text.contains("password"));

    // "restart": brand-new executor + registry, same store
    let restarted = executor(&store);
    assert!(
        restarted.resume_due(1_000_000 + 3_600_000).await.is_empty(),
        "not due yet"
    );
    assert_eq!(store.list().await.unwrap().len(), 1);

    let mut done = restarted.resume_due(1_000_000 + 2 * 3_600_000).await;
    assert_eq!(done.len(), 1);
    let done = done.remove(0).unwrap();
    assert_eq!(done.status, RunStatus::Success);
    assert_eq!(done.execution_id, "e-1");
    assert_eq!(
        out(&done, "After"),
        vec![json!({"k": 1, "before": true, "after": true})]
    );
    assert_eq!(
        out(&done, "Side"),
        vec![json!({"k": 1, "before": true, "side": true})]
    );
    // Wait forwarded its input unchanged and is recorded exactly once
    assert_eq!(done.run_data["Wait"].len(), 1);
    assert_eq!(out(&done, "Wait"), vec![json!({"k": 1, "before": true})]);
    assert!(
        store.list().await.unwrap().is_empty(),
        "claimed record is gone"
    );
    // nothing left to do twice
    assert!(restarted.resume_due(i64::MAX).await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn webhook_wait_resumes_with_request_item_and_checks_method() {
    let store = Arc::new(InMemoryWaitStore::new());
    let w = wf(
        json!({"resume": "webhook", "httpMethod": "POST", "options": {"webhookSuffix": "approve"}}),
    );
    let first = executor(&store).execute(&w, &start()).await.unwrap();
    assert_eq!(first.status, RunStatus::Waiting);
    assert_eq!(first.wait.unwrap().request.resume_at, None);

    let ex = executor(&store);
    let hit = vec![Item::from_json(json!({"body": {"approved": true}}))];
    // wrong method / suffix keep it parked
    assert!(
        ex.resume_webhook("e-1", "GET", Some("approve"), hit.clone())
            .await
            .is_err()
    );
    assert!(
        ex.resume_webhook("e-1", "POST", None, hit.clone())
            .await
            .is_err()
    );
    assert_eq!(store.list().await.unwrap().len(), 1);
    // unknown execution
    assert!(
        ex.resume_webhook("nope", "POST", Some("approve"), hit.clone())
            .await
            .unwrap()
            .is_none()
    );

    let done = ex
        .resume_webhook("e-1", "post", Some("approve"), hit)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, RunStatus::Success);
    // Wait outputs the webhook data, not its input
    assert_eq!(
        out(&done, "Wait"),
        vec![json!({"body": {"approved": true}})]
    );
    assert_eq!(
        out(&done, "After"),
        vec![json!({"body": {"approved": true}, "after": true})]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn webhook_wait_time_limit_resumes_on_timer() {
    let store = Arc::new(InMemoryWaitStore::new());
    let w = wf(
        json!({"resume": "webhook", "limitWaitTime": true, "limitType": "afterTimeInterval",
                      "resumeAmount": 30, "resumeUnit": "minutes"}),
    );
    let first = executor(&store).execute(&w, &start()).await.unwrap();
    assert_eq!(
        first.wait.unwrap().request.resume_at,
        Some(1_000_000 + 30 * 60_000)
    );
    let done = executor(&store)
        .resume_due(1_000_000 + 30 * 60_000)
        .await
        .remove(0)
        .unwrap();
    // timed out: continues with its input
    assert_eq!(out(&done, "Wait"), vec![json!({"k": 1, "before": true})]);
}

#[tokio::test(flavor = "current_thread")]
async fn second_wait_suspends_again_and_run_store_sees_waiting() {
    let runs = Arc::new(InMemoryRunStore::new());
    let store = Arc::new(InMemoryWaitStore::new());
    let w = Workflow::from_json_value(json!({
        "id": "wf-2", "name": "two waits",
        "nodes": [
            node("Trigger", "manualTrigger", 1.0, 0, json!({})),
            node("W1", "wait", 1.1, 200, json!({"resume": "timeInterval", "amount": 1, "unit": "hours"})),
            node("W2", "wait", 1.1, 400, json!({"resume": "timeInterval", "amount": 1, "unit": "hours"})),
            mark("End", 600, "end"),
        ],
        "connections": {
            "Trigger": {"main": [[{"node": "W1", "type": "main", "index": 0}]]},
            "W1": {"main": [[{"node": "W2", "type": "main", "index": 0}]]},
            "W2": {"main": [[{"node": "End", "type": "main", "index": 0}]]},
        }
    }))
    .unwrap();
    let ex = executor(&store).with_store(runs.clone());
    let a = ex.execute(&w, &start()).await.unwrap();
    assert_eq!(a.status, RunStatus::Waiting);
    use hk_flow::RunStore;
    assert_eq!(
        runs.load("e-1").await.unwrap().unwrap().status,
        RunStatus::Waiting
    );

    let b = ex
        .resume_due(1_000_000 + 3_600_000)
        .await
        .remove(0)
        .unwrap();
    assert_eq!(b.status, RunStatus::Waiting);
    assert!(b.run_data.contains_key("W1") && !b.run_data.contains_key("W2"));
    // the frozen test clock gives W2 the same due time, so one more tick finishes it
    let c = ex
        .resume_due(1_000_000 + 3_600_000)
        .await
        .remove(0)
        .unwrap();
    assert_eq!(c.status, RunStatus::Success);
    assert_eq!(out(&c, "End"), vec![json!({"k": 1, "end": true})]);
    assert_eq!(
        runs.load("e-1").await.unwrap().unwrap().status,
        RunStatus::Success
    );
}

#[tokio::test(flavor = "current_thread")]
async fn without_wait_store_long_wait_is_an_error_not_a_suspend() {
    let w = wf(json!({"resume": "webhook"}));
    let res = Executor::new(registry())
        .execute(&w, &start())
        .await
        .unwrap();
    assert_eq!(res.status, RunStatus::Error);
    assert!(res.error.unwrap().message.contains("durable wait"));
}

#[tokio::test(flavor = "current_thread")]
async fn resume_from_store_misses_cleanly() {
    let store = Arc::new(InMemoryWaitStore::new());
    let ex = executor(&store);
    assert!(
        ex.resume_from_store("ghost", ResumeReason::Elapsed)
            .await
            .unwrap()
            .is_none()
    );
    let bare = Executor::new(registry());
    assert!(matches!(
        bare.resume_from_store("x", ResumeReason::Elapsed).await,
        Err(FlowError::Invalid(_))
    ));
}
