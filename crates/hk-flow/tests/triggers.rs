//! Trigger host: activation, schedule ticks with a virtual clock, webhook dispatch,
//! app events, error workflows.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hk_flow::triggers::{
    AppEvent, FnSink, TriggerKind, TriggerMode, WebhookKind, WebhookRequest, error_trigger_item,
    find_trigger,
};
use hk_flow::{
    ExecuteInput, Executor, LocalTriggerHost, NodeRegistry, RunStatus, TriggerEvent, TriggerHost,
    WebhookRegistry, Workflow, register_core_nodes,
};
use serde_json::{Value, json};

fn collecting() -> (Arc<FnSink>, Arc<Mutex<Vec<TriggerEvent>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    (
        Arc::new(FnSink::new(move |e| s.lock().unwrap().push(e))),
        seen,
    )
}

fn host() -> (LocalTriggerHost, Arc<Mutex<Vec<TriggerEvent>>>) {
    let (sink, seen) = collecting();
    (
        LocalTriggerHost::new(sink, Arc::new(WebhookRegistry::new())),
        seen,
    )
}

fn wf(id: &str, trigger: Value, settings: Value) -> Workflow {
    Workflow::from_json_value(json!({
        "id": id, "name": format!("wf {id}"),
        "nodes": [
            trigger,
            {"name": "Next", "type": "n8n-nodes-base.noOp", "typeVersion": 1, "position": [200, 0], "parameters": {}}
        ],
        "connections": {},
        "settings": settings
    }))
    .unwrap()
}

fn schedule(params: Value) -> Value {
    json!({"name": "Sched", "type": "n8n-nodes-base.scheduleTrigger", "typeVersion": 1.2,
           "position": [0, 0], "parameters": params})
}

const T0: i64 = 1_704_067_200_000; // 2024-01-01T00:00:00Z

#[tokio::test(flavor = "current_thread")]
async fn schedule_fires_on_tick_and_rearms() {
    let (h, seen) = host();
    let w = wf(
        "s1",
        schedule(json!({"rule": {"interval": [{"field": "minutes", "minutesInterval": 5}]}})),
        json!({"timezone": "UTC"}),
    );
    let act = h.activate_at(&w, T0 + 1_000).await.unwrap();
    assert_eq!(act.schedules, 1);
    assert!(h.is_active("s1"));
    assert_eq!(h.next_fire_at(), Some(T0 + 300_000));

    assert_eq!(h.tick(T0 + 299_000), 0);
    assert_eq!(h.tick(T0 + 301_000), 1);
    assert_eq!(h.next_fire_at(), Some(T0 + 600_000));
    {
        let ev = seen.lock().unwrap();
        assert_eq!(ev[0].workflow_id, "s1");
        assert_eq!(ev[0].node_name.as_deref(), Some("Sched"));
        assert_eq!(ev[0].mode, TriggerMode::Trigger);
        // item carries the scheduled time, not the tick time
        assert_eq!(ev[0].items[0].json["Minute"], json!("05"));
    }

    // long sleep / restart: overdue beyond grace is dropped, not replayed, and re-armed
    assert_eq!(h.tick(T0 + 10 * 3_600_000), 0);
    assert_eq!(h.next_fire_at(), Some(T0 + 10 * 3_600_000 + 300_000));
    h.rearm(T0 + 11 * 3_600_000 + 1);
    assert_eq!(h.next_fire_at(), Some(T0 + 11 * 3_600_000 + 300_000));

    h.deactivate("s1").await.unwrap();
    assert!(!h.is_active("s1"));
    assert_eq!(h.next_fire_at(), None);
    assert_eq!(h.tick(T0 + 20 * 3_600_000), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn activation_rules() {
    let (h, _) = host();
    // manual-only workflows cannot be activated
    let manual = json!({"name": "M", "type": "n8n-nodes-base.manualTrigger", "typeVersion": 1,
                        "position": [0, 0], "parameters": {}});
    assert!(h.activate(&wf("m", manual, json!({}))).await.is_err());
    // bad schedule: nothing is left half-armed
    let bad =
        schedule(json!({"rule": {"interval": [{"field": "cronExpression", "expression": "x"}]}}));
    assert!(h.activate(&wf("b", bad, json!({}))).await.is_err());
    assert!(h.active_ids().is_empty());
    // webhook path clash between workflows is rejected, first one stays registered
    let hook = |p: &str| {
        json!({"name": "H", "type": "n8n-nodes-base.webhook", "typeVersion": 2, "position": [0, 0],
               "webhookId": "id", "parameters": {"path": p, "httpMethod": "POST"}})
    };
    h.activate(&wf("w1", hook("same"), json!({})))
        .await
        .unwrap();
    assert!(
        h.activate(&wf("w2", hook("same"), json!({})))
            .await
            .is_err()
    );
    assert!(!h.is_active("w2"));
    assert_eq!(h.webhooks().routes().len(), 1);
    h.deactivate("w1").await.unwrap();
    assert!(h.webhooks().routes().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn webhook_request_runs_through_executor() {
    let (h, seen) = host();
    let hook = json!({"name": "Hook", "type": "n8n-nodes-base.webhook", "typeVersion": 2,
                      "position": [0, 0], "webhookId": "abc",
                      "parameters": {"path": "in", "httpMethod": "POST"}});
    let w = wf("w", hook, json!({}));
    let act = h.activate(&w).await.unwrap();
    assert_eq!(act.webhooks[0].url_path(), "webhook/in");

    let mut req = WebhookRequest {
        method: "POST".into(),
        path: "in".into(),
        body: br#"{"who":"ada"}"#.to_vec(),
        ..WebhookRequest::default()
    };
    req.headers
        .push(("Content-Type".into(), "application/json".into()));
    let prepared = h.webhooks().prepare(WebhookKind::Production, &req).unwrap();
    h.dispatch_webhook(prepared);

    let ev = seen.lock().unwrap().remove(0);
    assert_eq!(ev.mode, TriggerMode::Webhook);
    // the daemon's job: event -> executor
    let mut reg = NodeRegistry::new();
    register_core_nodes(&mut reg).unwrap();
    let mut w = w;
    w.connections = serde_json::from_value(
        json!({"Hook": {"main": [[{"node": "Next", "type": "main", "index": 0}]]}}),
    )
    .unwrap();
    let res = Executor::new(Arc::new(reg))
        .execute(&w, &ExecuteInput::new(ev.node_name.unwrap(), ev.items))
        .await
        .unwrap();
    assert_eq!(res.status, RunStatus::Success);
    assert_eq!(
        res.run_data["Next"][0].output(0)[0].json["body"],
        json!({"who": "ada"})
    );
}

#[tokio::test(flavor = "current_thread")]
async fn app_event_trigger_matches_pattern() {
    let (h, seen) = host();
    let t = json!({"name": "Ev", "type": "hokkabaz.appEventTrigger", "typeVersion": 1,
                   "position": [0, 0], "parameters": {"event": "session/*"}});
    h.activate(&wf("e", t, json!({}))).await.unwrap();
    assert_eq!(
        h.bus().publish(AppEvent {
            name: "other/x".into(),
            payload: json!(1)
        }),
        1
    );
    h.bus().publish(AppEvent {
        name: "session/finished".into(),
        payload: json!({"id": "s9"}),
    });
    for _ in 0..100 {
        if !seen.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let ev = seen.lock().unwrap();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].items[0].json["event"], json!("session/finished"));
    assert_eq!(ev[0].items[0].json["payload"], json!({"id": "s9"}));
}

#[tokio::test(flavor = "current_thread")]
async fn error_workflow_gets_error_trigger_item() {
    let (h, seen) = host();
    let mut failing = wf(
        "f",
        json!({"name": "M", "type": "n8n-nodes-base.manualTrigger", "typeVersion": 1, "position": [0, 0], "parameters": {}}),
        json!({"errorWorkflow": "handler"}),
    );
    failing.nodes.push(
        serde_json::from_value(json!({"name": "Boom", "type": "n8n-nodes-base.doesNotExist",
                                      "typeVersion": 1, "position": [400, 0], "parameters": {"password": "s3cret"}}))
            .unwrap(),
    );
    failing.connections = serde_json::from_value(
        json!({"M": {"main": [[{"node": "Boom", "type": "main", "index": 0}]]}}),
    )
    .unwrap();
    let mut reg = NodeRegistry::new();
    register_core_nodes(&mut reg).unwrap();
    let res = Executor::new(Arc::new(reg))
        .execute(
            &failing,
            &ExecuteInput::new("M", vec![hk_flow::Item::default()]),
        )
        .await
        .unwrap();
    assert_eq!(res.status, RunStatus::Error);
    assert!(h.on_execution_finished(&failing, &res, Some("https://hk.example")));
    let ev = seen.lock().unwrap().remove(0);
    assert_eq!(
        (ev.workflow_id.as_str(), ev.mode),
        ("handler", TriggerMode::Error)
    );
    assert!(ev.node_name.is_none());
    let item = Value::Object(ev.items[0].json.clone());
    assert_eq!(item["workflow"], json!({"id": "f", "name": "wf f"}));
    assert_eq!(item["execution"]["lastNodeExecuted"], json!("Boom"));
    assert_eq!(item["execution"]["mode"], json!("trigger"));
    assert!(
        item["execution"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not supported")
    );
    assert!(!item.to_string().contains("s3cret"));

    // no loop: a workflow that names itself as its error workflow stays quiet
    failing.settings.as_mut().unwrap().error_workflow = Some("f".into());
    assert!(!h.on_execution_finished(&failing, &res, None));
    // the builder alone, and trigger lookup for the receiving side
    let _ = error_trigger_item(&failing, &res, "manual", None);
    let handler = wf(
        "handler",
        json!({"name": "Err", "type": "n8n-nodes-base.errorTrigger", "typeVersion": 1, "position": [0, 0], "parameters": {}}),
        json!({}),
    );
    assert_eq!(
        find_trigger(&handler, TriggerKind::Error).unwrap().name,
        "Err"
    );
}

#[cfg(all(feature = "notify", not(target_os = "android")))]
#[tokio::test(flavor = "current_thread")]
async fn file_watch_emits_create_events() {
    let dir = std::env::temp_dir().join(format!("hk-flow-watch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (h, seen) = host();
    let t = json!({"name": "FW", "type": "hokkabaz.fileWatchTrigger", "typeVersion": 1, "position": [0, 0],
                   "parameters": {"path": dir.to_str().unwrap(), "events": ["create"]}});
    let act = h.activate(&wf("fw", t, json!({}))).await.unwrap();
    assert_eq!(act.file_watches, 1);
    std::fs::write(dir.join("a.txt"), b"x").unwrap();
    for _ in 0..200 {
        if !seen.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    h.deactivate("fw").await.unwrap();
    let ev = seen.lock().unwrap();
    assert!(!ev.is_empty(), "no file event");
    assert_eq!(ev[0].items[0].json["event"], json!("create"));
    assert!(
        ev[0].items[0].json["path"]
            .as_str()
            .unwrap()
            .ends_with("a.txt")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(not(all(feature = "notify", not(target_os = "android"))))]
#[tokio::test(flavor = "current_thread")]
async fn file_watch_without_feature_is_a_clean_error() {
    let (h, _) = host();
    let t = json!({"name": "FW", "type": "hokkabaz.fileWatchTrigger", "typeVersion": 1, "position": [0, 0],
                   "parameters": {"path": "/tmp"}});
    let e = h.activate(&wf("fw", t, json!({}))).await.unwrap_err();
    assert!(e.to_string().contains("notify"));
    assert!(!h.is_active("fw"));
}
