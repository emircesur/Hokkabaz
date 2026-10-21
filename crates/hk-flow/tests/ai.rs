//! AI nodes: sub-node resolution, memory, tool wiring, secrets hygiene. Offline: models are
//! scripted. Corpus-style end-to-end cases live in `tests/n8n/cases/ai_*`.
#![cfg(feature = "ai")]

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use hk_flow::nodes::ai::{
    AgentRunRequest, AgentRunResult, AgentRunner, ChatModel, ChatModelFactory, ChatModelSpec,
    HkLlmChatModelFactory, ScriptedChatModelFactory,
};
use hk_flow::{
    CredentialRef, CredentialResolver, ExecuteInput, Executor, ExecutorConfig, FlowError, Item,
    NodeHooks, NodeRegistry, RunStatus, SubWorkflowRequest, SubWorkflowRunner, Workflow,
    register_core_nodes,
};
use serde_json::{Map, Value, json};

fn executor(hooks: NodeHooks) -> Executor {
    let mut r = NodeRegistry::new();
    register_core_nodes(&mut r).unwrap();
    Executor::new(Arc::new(r)).with_config(ExecutorConfig {
        hooks,
        ..ExecutorConfig::default()
    })
}

fn node(name: &str, ty: &str, ver: f64, params: Value) -> Value {
    json!({"name": name, "type": ty, "typeVersion": ver, "position": [0, 0], "parameters": params})
}

fn lm(name: &str, model: &str) -> Value {
    node(
        name,
        "@n8n/n8n-nodes-langchain.lmChatOpenAi",
        1.2,
        json!({"model": model}),
    )
}

async fn run(ex: &Executor, wf: Value, items: Vec<Value>) -> hk_flow::ExecutionResult {
    let wf = Workflow::from_json_value(wf).unwrap();
    let items = items.into_iter().map(Item::from_json).collect();
    ex.execute(&wf, &ExecuteInput::new("Trigger", items))
        .await
        .unwrap()
}

fn trigger() -> Value {
    node("Trigger", "n8n-nodes-base.manualTrigger", 1.0, json!({}))
}

fn out_json(res: &hk_flow::ExecutionResult, node: &str) -> Value {
    let runs = &res.run_data[node];
    serde_json::to_value(&runs[0].data.main[0]).unwrap()
}

fn agent_wf(extra_nodes: Vec<Value>, extra_conns: Value) -> Value {
    let mut nodes = vec![
        trigger(),
        node(
            "Agent",
            "@n8n/n8n-nodes-langchain.agent",
            1.7,
            json!({"promptType": "auto", "options": {"maxIterations": 3}}),
        ),
        lm("Model", "m1"),
    ];
    nodes.extend(extra_nodes);
    let mut conns = json!({
        "Trigger": {"main": [[{"node": "Agent", "type": "main", "index": 0}]]},
        "Model": {"ai_languageModel": [[{"node": "Agent", "type": "ai_languageModel", "index": 0}]]},
    });
    for (k, v) in extra_conns.as_object().unwrap() {
        conns[k] = v.clone();
    }
    json!({"name": "t", "nodes": nodes, "connections": conns})
}

#[tokio::test]
async fn memory_carries_history_across_executions() {
    let script = ScriptedChatModelFactory::new();
    script.push_text("m1", "Hi Ada");
    script.push_text("m1", "Your name is Ada");
    let ex = executor(NodeHooks {
        chat_models: Some(Arc::new(script.clone())),
        ..NodeHooks::default()
    });
    let wf = agent_wf(
        vec![node(
            "Mem",
            "@n8n/n8n-nodes-langchain.memoryBufferWindow",
            1.3,
            json!({"contextWindowLength": 5}),
        )],
        json!({"Mem": {"ai_memory": [[{"node": "Agent", "type": "ai_memory", "index": 0}]]}}),
    );
    for q in ["I am Ada", "Who am I?"] {
        let res = run(
            &ex,
            wf.clone(),
            vec![json!({"chatInput": q, "sessionId": "s1"})],
        )
        .await;
        assert_eq!(res.status, RunStatus::Success, "{:?}", res.error);
    }
    // a different session sees none of it
    run(
        &ex,
        wf.clone(),
        vec![json!({"chatInput": "x", "sessionId": "s2"})],
    )
    .await;
    let seen = script.seen();
    // third run has no scripted reply (it fails) but its request shows no foreign history
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[2].messages.len(), 2);
    let second: Vec<_> = seen[1]
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(second[1..], ["I am Ada", "Hi Ada", "Who am I?"]);
}

#[derive(Debug)]
struct EchoRunner(Mutex<Vec<SubWorkflowRequest>>);
#[async_trait]
impl SubWorkflowRunner for EchoRunner {
    async fn run(&self, req: SubWorkflowRequest) -> Result<Vec<Item>, FlowError> {
        let json = req.items[0].json.clone();
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(req);
        let mut out = Map::new();
        out.insert("shipped".into(), json!(true));
        out.insert("saw".into(), Value::Object(json));
        Ok(vec![Item::new(out)])
    }
}

#[tokio::test]
async fn workflow_tool_maps_from_ai_args_and_hides_expressions() {
    let script = ScriptedChatModelFactory::from_json(&json!({"models": {"m1": [
        {"tool_calls": [{"name": "lookup", "arguments": {"orderId": "={{ 1 + 1 }}"}}]},
        {"text": "done", "expect_last_contains": "shipped"}
    ]}}))
    .unwrap();
    let runner = Arc::new(EchoRunner(Mutex::new(Vec::new())));
    let ex = executor(NodeHooks {
        chat_models: Some(Arc::new(script.clone())),
        sub_workflow: Some(runner.clone()),
        ..NodeHooks::default()
    });
    let wf = agent_wf(
        vec![node(
            "Order lookup",
            "@n8n/n8n-nodes-langchain.toolWorkflow",
            1.2,
            json!({
                "name": "lookup", "description": "find an order",
                "workflowId": {"__rl": true, "value": "wf-7", "mode": "id"},
                "workflowInputs": {"mappingMode": "defineBelow",
                    "value": {"orderId": "={{ $fromAI('orderId', 'the order id', 'string') }}"}}
            }),
        )],
        json!({"Order lookup": {"ai_tool": [[{"node": "Agent", "type": "ai_tool", "index": 0}]]}}),
    );
    let res = run(&ex, wf, vec![json!({"chatInput": "where is it"})]).await;
    assert_eq!(res.status, RunStatus::Success, "{:?}", res.error);
    assert_eq!(
        out_json(&res, "Agent"),
        json!([{"json": {"output": "done"}, "pairedItem": 0}])
    );
    let reqs = runner.0.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(reqs[0].workflow_id.as_deref(), Some("wf-7"));
    // the model's string reached the sub-workflow verbatim, never evaluated
    assert_eq!(reqs[0].items[0].json["orderId"], json!(" ={{ 1 + 1 }}"));
    // tool schema offered to the model came from the $fromAI call
    let tools = &script.seen()[0].tools;
    assert_eq!(tools[0].name, "lookup");
    assert_eq!(
        tools[0].parameters["properties"]["orderId"]["description"],
        "the order id"
    );
}

#[tokio::test]
async fn max_iterations_stops_a_looping_agent() {
    let calls: Vec<Value> = (0..5)
        .map(|_| json!({"tool_calls": [{"name": "nope", "arguments": {}}]}))
        .collect();
    let script = ScriptedChatModelFactory::from_json(&json!({"models": {"m1": calls}})).unwrap();
    let ex = executor(NodeHooks {
        chat_models: Some(Arc::new(script)),
        ..NodeHooks::default()
    });
    let res = run(
        &ex,
        agent_wf(vec![], json!({})),
        vec![json!({"chatInput": "go"})],
    )
    .await;
    assert_eq!(res.status, RunStatus::Error);
    assert!(res.error.unwrap().message.contains("max iterations"));
}

#[tokio::test]
async fn missing_model_is_a_clear_error() {
    let ex = executor(NodeHooks::default());
    let wf = json!({"name": "t", "nodes": [trigger(), node("Agent", "@n8n/n8n-nodes-langchain.agent", 1.7, json!({"promptType": "define", "text": "hi"}))],
        "connections": {"Trigger": {"main": [[{"node": "Agent", "type": "main", "index": 0}]]}}});
    let res = run(&ex, wf, vec![json!({})]).await;
    assert_eq!(res.status, RunStatus::Error);
    assert!(
        res.error
            .unwrap()
            .message
            .contains("Chat Model is required")
    );
}

#[derive(Debug)]
struct Creds;
#[async_trait]
impl CredentialResolver for Creds {
    async fn resolve(&self, ty: &str, _r: &CredentialRef) -> Result<Map<String, Value>, FlowError> {
        assert_eq!(ty, "openAiApi");
        Ok(
            json!({"apiKey": "sk-secret-123", "url": "http://localhost:1/v1"})
                .as_object()
                .unwrap()
                .clone(),
        )
    }
}

/// Records the spec it was asked to build, then delegates to the script.
#[derive(Debug)]
struct Spy(ScriptedChatModelFactory, Mutex<Vec<ChatModelSpec>>);
#[async_trait]
impl ChatModelFactory for Spy {
    async fn build(&self, spec: ChatModelSpec) -> Result<Arc<dyn ChatModel>, FlowError> {
        self.1
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(spec.clone());
        self.0.build(spec).await
    }
}

#[tokio::test]
async fn credentials_reach_factory_but_not_execution_data() {
    let script = ScriptedChatModelFactory::new();
    script.push_text("m1", "ok");
    let spy = Arc::new(Spy(script, Mutex::new(Vec::new())));
    let ex = executor(NodeHooks {
        chat_models: Some(spy.clone()),
        credentials: Some(Arc::new(Creds)),
        ..NodeHooks::default()
    });
    let mut wf = agent_wf(vec![], json!({}));
    wf["nodes"][2]["credentials"] = json!({"openAiApi": {"id": "c1", "name": "OpenAI"}});
    let res = run(&ex, wf, vec![json!({"chatInput": "hi"})]).await;
    assert_eq!(res.status, RunStatus::Success, "{:?}", res.error);
    let specs = spy.1.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(specs[0].api_key.as_deref(), Some("sk-secret-123"));
    assert_eq!(specs[0].base_url.as_deref(), Some("http://localhost:1/v1"));
    assert!(!format!("{:?}", specs[0]).contains("sk-secret"));
    let dump = serde_json::to_string(&res.run_data).unwrap();
    assert!(!dump.contains("sk-secret"));
}

#[tokio::test]
async fn default_factory_builds_providers_offline() {
    let spec = |provider: &str, key: Option<&str>| ChatModelSpec {
        provider: provider.into(),
        model: "m".into(),
        api_key: key.map(Into::into),
        ..ChatModelSpec::default()
    };
    for p in ["openai", "openrouter", "deepseek", "anthropic", "gemini"] {
        let m = HkLlmChatModelFactory
            .build(spec(p, Some("k")))
            .await
            .unwrap();
        assert_eq!(m.model_name(), "m");
    }
    assert!(
        HkLlmChatModelFactory
            .build(spec("ollama", None))
            .await
            .is_ok()
    );
    let err = HkLlmChatModelFactory
        .build(spec("deepseek", None))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("missing API key"), "{err}");
    assert!(
        HkLlmChatModelFactory
            .build(spec("nope", None))
            .await
            .is_err()
    );
}

#[derive(Debug)]
struct Runner;
#[async_trait]
impl AgentRunner for Runner {
    async fn run(&self, req: AgentRunRequest) -> Result<AgentRunResult, FlowError> {
        Ok(AgentRunResult {
            output: format!("{}|{:?}|{:?}", req.prompt, req.mode, req.tools),
            session_id: Some("sess-1".into()),
            ..AgentRunResult::default()
        })
    }
}

#[tokio::test]
async fn native_agent_uses_runner_hook() {
    let ex = executor(NodeHooks {
        agent_runner: Some(Arc::new(Runner)),
        ..NodeHooks::default()
    });
    let wf = json!({"name": "t", "nodes": [trigger(), node("A", "hokkabaz.agent", 1.0,
        json!({"prompt": "=fix {{ $json.bug }}", "mode": "plan", "tools": "read, grep"}))],
        "connections": {"Trigger": {"main": [[{"node": "A", "type": "main", "index": 0}]]}}});
    let res = run(&ex, wf, vec![json!({"bug": "#12"})]).await;
    assert_eq!(res.status, RunStatus::Success, "{:?}", res.error);
    assert_eq!(
        res.run_data["A"][0].data.main[0].as_ref().unwrap()[0].json["output"],
        json!("fix #12|Some(\"plan\")|[\"read\", \"grep\"]")
    );
}

// ------------------------------------------------------------------- http tool

/// One canned response, returns the raw request.
#[cfg(feature = "http")]
async fn serve_once(resp: &str) -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (seen2, resp) = (seen.clone(), resp.to_owned());
    tokio::spawn(async move {
        let Ok((mut sock, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        seen2
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(String::from_utf8_lossy(&buf[..n]).into_owned());
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.shutdown().await;
    });
    (format!("http://{addr}"), seen)
}

#[cfg(feature = "http")]
#[tokio::test]
async fn http_tool_fills_placeholders_and_model_fields() {
    let (base, seen) = serve_once(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 19\r\nConnection: close\r\n\r\n{\"temp_c\":21,\"x\":1}",
    )
    .await;
    let script = ScriptedChatModelFactory::from_json(&json!({"models": {"m1": [
        {"tool_calls": [{"name": "weather", "arguments": {"city": "São Paulo", "units": "metric"}}]},
        {"text": "21C", "expect_last_contains": "temp_c"}
    ]}}))
    .unwrap();
    let ex = executor(NodeHooks {
        chat_models: Some(Arc::new(script.clone())),
        ..NodeHooks::default()
    });
    let wf = agent_wf(
        vec![node(
            "Weather",
            "@n8n/n8n-nodes-langchain.toolHttpRequest",
            1.1,
            json!({
                "name": "weather", "toolDescription": "current weather", "method": "GET",
                "url": format!("{base}/w/{{city}}"),
                "placeholderDefinitions": {"values": [{"name": "city", "description": "City name", "type": "string"}]},
                "sendQuery": true, "specifyQuery": "keypair",
                "parametersQuery": {"values": [
                    {"name": "units", "valueProvider": "modelRequired", "value": "metric or imperial"},
                    {"name": "lang", "valueProvider": "fieldValue", "value": "en"}]}
            }),
        )],
        json!({"Weather": {"ai_tool": [[{"node": "Agent", "type": "ai_tool", "index": 0}]]}}),
    );
    let res = run(&ex, wf, vec![json!({"chatInput": "weather?"})]).await;
    assert_eq!(res.status, RunStatus::Success, "{:?}", res.error);
    let req = seen.lock().unwrap_or_else(PoisonError::into_inner)[0].clone();
    let line = req.lines().next().unwrap();
    assert!(line.starts_with("GET /w/S%C3%A3o%20Paulo?"), "{line}");
    assert!(
        line.contains("units=metric") && line.contains("lang=en"),
        "{line}"
    );
    let t = &script.seen()[0].tools[0];
    assert_eq!(t.parameters["required"], json!(["city", "units"]));
}
