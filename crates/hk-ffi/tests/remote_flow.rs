//! `RemoteCore` test runs against the real hk-server surface on loopback: the app's workflow is
//! uploaded, run through `POST /workflows/{id}/run`, and read back as events / list / detail.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use hk_ffi::dto::{CoreProfile, FlowRunEventKind as K, FlowRunModeDto};
use hk_ffi::handle::CoreHandle;
use hk_ffi::remote::RemoteCore;
use hk_flow::{ExecuteInput, ExecutorConfig, Executor, NodeRegistry, register_core_nodes};
use hk_server::{AppState, Auth, FlowApi, FlowToolHost, LocalConfig, LocalRuntime, Scope, TestRun, serve_listener};
use hk_store::{MemorySecretStore, Secret, Store};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const ADMIN: &str = "admin-token-0123456789";

/// What hokkabazd's flow runtime does for `run_test`, minus triggers and credentials.
#[derive(Debug)]
struct Host {
    api: FlowApi,
    exec: Executor,
    nodes: Arc<NodeRegistry>,
}

#[async_trait::async_trait]
impl FlowToolHost for Host {
    fn nodes(&self) -> &NodeRegistry {
        &self.nodes
    }

    async fn run_test(&self, req: TestRun) -> Result<hk_flow::ExecutionResult, String> {
        let wid = req.workflow_id.clone().unwrap();
        let id = self.api.start_execution(&wid, "manual", None).await.map_err(|e| e.to_string())?;
        let exec = self.exec.clone().with_config(ExecutorConfig { use_pin_data: true, ..ExecutorConfig::default() });
        let mut input = ExecuteInput::new(req.start_node, req.items);
        input.execution_id = Some(id.to_string());
        let res = exec.execute(&req.workflow, &input).await.map_err(|e| e.to_string())?;
        let data = json!({ "resultData": { "runData": serde_json::to_value(&res.run_data).unwrap() } });
        self.api.finish_execution(id, "success", Some(&data)).await.map_err(|e| e.to_string())?;
        Ok(res)
    }
}

fn workflow(set_value: i64) -> String {
    json!({
        "id": "local-1",
        "name": "Remote set",
        "nodes": [
            {"id": "1", "name": "Trigger", "type": "n8n-nodes-base.manualTrigger", "typeVersion": 1, "position": [0, 0], "parameters": {}},
            {"id": "2", "name": "Set", "type": "n8n-nodes-base.set", "typeVersion": 3.4, "position": [220, 0],
             "parameters": {"mode": "raw", "jsonOutput": format!("{{\"a\": {set_value}}}"), "includeOtherFields": false, "options": {}}}
        ],
        "connections": {"Trigger": {"main": [[{"node": "Set", "type": "main", "index": 0}]]}},
        "settings": {"executionOrder": "v1"}
    })
    .to_string()
}

#[tokio::test]
async fn upload_run_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    struct Quiet;
    #[async_trait::async_trait]
    impl hk_llm::Provider for Quiet {
        fn id(&self) -> &str {
            "fake"
        }
        async fn models(&self) -> hk_llm::Result<Vec<hk_llm::ModelInfo>> {
            Ok(Vec::new())
        }
        async fn stream(&self, _r: hk_llm::ChatRequest, _c: hk_llm::CancellationToken) -> hk_llm::Result<hk_llm::EventStream> {
            Ok(futures::stream::empty().boxed())
        }
    }
    let runtime = Arc::new(
        LocalRuntime::new(LocalConfig {
            provider: Arc::new(Quiet),
            sessions_dir: dir.path().join("sessions"),
            spill_dir: dir.path().join("spill"),
            default_model: "m".into(),
            default_cwd: Some(dir.path().to_path_buf()),
            max_steps: 4,
        })
        .unwrap(),
    );
    let store = Store::open_in_memory().await.unwrap();
    let base = FlowApi::new(store, Arc::new(MemorySecretStore::default()));
    let mut reg = NodeRegistry::new();
    register_core_nodes(&mut reg).unwrap();
    let nodes = Arc::new(reg);
    let host = Arc::new(Host { api: base.clone(), exec: Executor::new(nodes.clone()), nodes });
    let flow = base.with_test_runner(host);
    let auth = Auth::new().with_token(Scope::Admin, ADMIN);
    tokio::spawn(serve_listener(listener, AppState::new(runtime, auth).with_flow(flow), std::future::pending()));

    let core = RemoteCore::new(&url, Secret::new(ADMIN), CoreProfile::Desktop).unwrap();
    let id = core.run_workflow(workflow(1), FlowRunModeDto::Test, None, None, None).await.unwrap();
    let evs = tokio::time::timeout(Duration::from_secs(10), core.run_events(&id).unwrap().collect::<Vec<_>>()).await.unwrap();
    assert_eq!(evs.first().map(|e| e.kind), Some(K::RunStarted));
    assert_eq!(evs.last().map(|e| e.kind), Some(K::RunFinished));
    let set = evs.iter().find(|e| e.kind == K::NodeFinished && e.node.as_deref() == Some("Set")).unwrap();
    let out: Value = serde_json::from_str(set.output_json.as_deref().unwrap()).unwrap();
    assert_eq!(out[0][0]["json"]["a"], 1);

    // Second run of the same app workflow updates the one uploaded copy, not a second one.
    let id2 = core.run_workflow(workflow(2), FlowRunModeDto::Test, None, None, None).await.unwrap();
    assert_ne!(id, id2);
    let runs = core.list_runs(Some("local-1".into()), 10).await.unwrap();
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|r| r.remote && r.workflow_name == "Remote set" && r.status == "success"));
    assert_eq!(runs[0].id, id2, "newest first");

    let d = core.get_run(&id2).await.unwrap();
    assert!(d.run_data_json.contains("\"a\":2"));
    assert!(d.workflow_json.unwrap().contains("Remote set"));

    // Pin override: Manual ignores stored/override pins, Test applies the override.
    let pin = json!({"Set": [{"json": {"pinned": true}}]}).to_string();
    let id3 = core.run_workflow(workflow(3), FlowRunModeDto::Test, None, None, Some(pin.clone())).await.unwrap();
    assert!(core.get_run(&id3).await.unwrap().run_data_json.contains("pinned"));
    let id4 = core.run_workflow(workflow(4), FlowRunModeDto::Manual, None, None, Some(pin)).await.unwrap();
    assert!(core.get_run(&id4).await.unwrap().run_data_json.contains("\"a\":4"));

    // No activator on this instance: the flag flips on the stored row.
    assert!(core.set_workflow_active(workflow(5), true).await.unwrap());
    assert!(!core.set_workflow_active(workflow(5), false).await.unwrap());
}
