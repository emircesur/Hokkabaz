//! Core nodes beyond the corpus: executor expression wiring, hooks (Respond to Webhook,
//! Execute Workflow, credentials), HTTP Request against a local server, Wait.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hk_flow::expr::default_evaluator;
use hk_flow::{
    CollectingSink, CredentialRef, CredentialResolver, ExecuteInput, Executor, ExecutorConfig,
    ExprError, ExprScope, ExpressionEvaluator, FlowError, Item, NodeHooks, NodeRegistry,
    ResponseBody, SubWorkflowRequest, SubWorkflowRunner, Workflow, register_core_nodes,
};
use serde_json::{Map, Value, json};

fn registry() -> Arc<NodeRegistry> {
    let mut r = NodeRegistry::new();
    register_core_nodes(&mut r).unwrap();
    Arc::new(r)
}

/// Trigger -> one node; returns the workflow.
fn single(node_type: &str, version: f64, params: Value, extra: Value, settings: Value) -> Workflow {
    let mut node = json!({
        "name": "N", "type": format!("n8n-nodes-base.{node_type}"), "typeVersion": version,
        "position": [200, 0], "parameters": params
    });
    if let (Some(n), Some(e)) = (node.as_object_mut(), extra.as_object()) {
        n.extend(e.clone());
    }
    Workflow::from_json_value(json!({
        "name": "t",
        "nodes": [
            {"name": "Trigger", "type": "n8n-nodes-base.manualTrigger", "typeVersion": 1, "position": [0, 0], "parameters": {}},
            node
        ],
        "connections": {"Trigger": {"main": [[{"node": "N", "type": "main", "index": 0}]]}},
        "settings": settings
    }))
    .unwrap()
}

fn items(v: Value) -> Vec<Item> {
    v.as_array()
        .unwrap()
        .iter()
        .map(Item::from_pinned)
        .collect()
}

fn out_json(res: &hk_flow::ExecutionResult, node: &str, output: usize) -> Vec<Value> {
    res.run_data[node][0]
        .output(output)
        .iter()
        .map(|i| Value::Object(i.json.clone()))
        .collect()
}

// ------------------------------------------------------------ expression wiring

#[derive(Debug)]
struct Recording {
    inner: Arc<dyn ExpressionEvaluator>,
    released: Mutex<Vec<String>>,
}

impl ExpressionEvaluator for Recording {
    fn evaluate(&self, template: &str, scope: &ExprScope) -> Result<Value, ExprError> {
        self.inner.evaluate(template, scope)
    }
    fn release(&self, id: &str) {
        self.released.lock().unwrap().push(id.to_owned());
        self.inner.release(id);
    }
}

fn expr_set(fields: &[(&str, &str)]) -> Value {
    let a: Vec<Value> = fields
        .iter()
        .map(|(n, v)| json!({"id": n, "name": n, "value": v, "type": "string"}))
        .collect();
    json!({"mode": "manual", "assignments": {"assignments": a}, "includeOtherFields": false, "options": {}})
}

#[tokio::test(flavor = "current_thread")]
async fn executor_populates_expression_env_and_releases() {
    let rec = Arc::new(Recording {
        inner: default_evaluator(),
        released: Mutex::new(Vec::new()),
    });
    let wf = single(
        "set",
        3.4,
        expr_set(&[
            ("mode", "={{ $execution.mode }}"),
            ("tz", "={{ $now.toFormat('ZZ') }}"),
            ("own", "={{ $node.N.parameter.mode }}"),
            ("var", "={{ $vars.team }}"),
            ("path", "={{ typeof $env.PATH }}"),
            ("wf", "={{ $workflow.name }}"),
        ]),
        json!({}),
        json!({"executionOrder": "v1", "timezone": "Europe/Istanbul"}),
    );
    let mut vars = Map::new();
    vars.insert("team".into(), json!("core"));
    let cfg = ExecutorConfig {
        mode: "webhook".into(),
        vars,
        evaluator: Some(rec.clone()),
        frozen_now_ms: Some(1_700_000_000_000),
        ..ExecutorConfig::default()
    };
    let exec = Executor::new(registry()).with_config(cfg);
    let mut input = ExecuteInput::new("Trigger", items(json!([{"a": 1}])));
    input.execution_id = Some("exec-x".into());
    let res = exec.execute(&wf, &input).await.unwrap();
    assert!(res.error.is_none(), "{:?}", res.error);
    let o = &out_json(&res, "N", 0)[0];
    assert_eq!(o["mode"], "webhook");
    assert_eq!(o["tz"], "+03:00");
    assert_eq!(o["own"], "manual");
    assert_eq!(o["var"], "core");
    assert_eq!(o["wf"], "t");
    // env is empty unless allowlisted
    assert_eq!(o["path"], "undefined");
    assert_eq!(*rec.released.lock().unwrap(), vec!["exec-x".to_owned()]);
}

#[tokio::test(flavor = "current_thread")]
async fn env_allowlist_opts_in() {
    let wf = single(
        "set",
        3.4,
        expr_set(&[
            ("p", "={{ typeof $env.PATH }}"),
            ("h", "={{ typeof $env.HOME }}"),
        ]),
        json!({}),
        json!({}),
    );
    let cfg = ExecutorConfig {
        env_allowlist: vec!["PATH".into()],
        ..ExecutorConfig::default()
    };
    let res = Executor::new(registry())
        .with_config(cfg)
        .execute(&wf, &ExecuteInput::new("Trigger", items(json!([{}]))))
        .await
        .unwrap();
    let o = &out_json(&res, "N", 0)[0];
    assert_eq!(o["p"], "string");
    assert_eq!(o["h"], "undefined");
}

// ---------------------------------------------------------------------- hooks

#[tokio::test(flavor = "current_thread")]
async fn respond_to_webhook_emits_to_sink() {
    let sink = Arc::new(CollectingSink::default());
    let wf = single(
        "respondToWebhook",
        1.3,
        json!({
            "respondWith": "allIncomingItems",
            "options": {"responseCode": 201, "responseKey": "rows",
                "responseHeaders": {"entries": [{"name": "X-Count", "value": "={{ 1 + 1 }}"}]}}
        }),
        json!({}),
        json!({}),
    );
    let cfg = ExecutorConfig {
        hooks: NodeHooks {
            response: Some(sink.clone()),
            ..NodeHooks::default()
        },
        ..ExecutorConfig::default()
    };
    let res = Executor::new(registry())
        .with_config(cfg)
        .execute(
            &wf,
            &ExecuteInput::new("Trigger", items(json!([{"a": 1}, {"a": 2}]))),
        )
        .await
        .unwrap();
    // items pass through
    assert_eq!(out_json(&res, "N", 0).len(), 2);
    let got = sink.take();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].status, 201);
    assert_eq!(got[0].headers, vec![("X-Count".to_owned(), "2".to_owned())]);
    assert_eq!(
        got[0].body,
        ResponseBody::Json(json!({"rows": [{"a": 1}, {"a": 2}]}))
    );
}

#[derive(Debug)]
struct StubRunner(Mutex<Vec<(Option<String>, usize)>>);

#[async_trait]
impl SubWorkflowRunner for StubRunner {
    async fn run(&self, req: SubWorkflowRequest) -> Result<Vec<Item>, FlowError> {
        self.0
            .lock()
            .unwrap()
            .push((req.workflow_id.clone(), req.items.len()));
        Ok(req
            .items
            .into_iter()
            .map(|mut i| {
                i.json.insert("child".into(), json!(true));
                i
            })
            .collect())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn execute_workflow_uses_runner_hook() {
    let runner = Arc::new(StubRunner(Mutex::new(Vec::new())));
    let wf = single(
        "executeWorkflow",
        1.2,
        json!({"source": "database", "workflowId": {"__rl": true, "value": "wf-7", "mode": "id"}, "mode": "each"}),
        json!({}),
        json!({}),
    );
    let cfg = ExecutorConfig {
        hooks: NodeHooks {
            sub_workflow: Some(runner.clone()),
            ..NodeHooks::default()
        },
        ..ExecutorConfig::default()
    };
    let exec = Executor::new(registry()).with_config(cfg);
    let res = exec
        .execute(
            &wf,
            &ExecuteInput::new("Trigger", items(json!([{"a": 1}, {"a": 2}]))),
        )
        .await
        .unwrap();
    assert_eq!(
        out_json(&res, "N", 0),
        vec![
            json!({"a": 1, "child": true}),
            json!({"a": 2, "child": true})
        ]
    );
    assert_eq!(runner.0.lock().unwrap().len(), 2);

    // no hook: clear error, not a panic
    let res = Executor::new(registry())
        .execute(&wf, &ExecuteInput::new("Trigger", items(json!([{}]))))
        .await
        .unwrap();
    assert!(res.error.unwrap().message.contains("not available"));
}

// ----------------------------------------------------------------------- wait

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn wait_sleeps_inline_and_rejects_long() {
    let wf = single(
        "wait",
        1.1,
        json!({"resume": "timeInterval", "amount": 5, "unit": "seconds"}),
        json!({}),
        json!({}),
    );
    let t0 = tokio::time::Instant::now();
    let res = Executor::new(registry())
        .execute(&wf, &ExecuteInput::new("Trigger", items(json!([{"k": 1}]))))
        .await
        .unwrap();
    assert!(res.error.is_none());
    assert!(t0.elapsed() >= Duration::from_secs(5));
    assert_eq!(out_json(&res, "N", 0), vec![json!({"k": 1})]);

    let long = single(
        "wait",
        1.1,
        json!({"resume": "timeInterval", "amount": 2, "unit": "hours"}),
        json!({}),
        json!({}),
    );
    let res = Executor::new(registry())
        .execute(&long, &ExecuteInput::new("Trigger", items(json!([{}]))))
        .await
        .unwrap();
    assert!(res.error.unwrap().message.contains("durable wait"));
}

// ------------------------------------------------------------------------ http

/// Serves one canned raw response per connection; records raw requests.
async fn serve(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        for resp in responses {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).into_owned();
                if let Some(h) = text.find("\r\n\r\n") {
                    let len = text[..h]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if buf.len() >= h + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            seen2
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf).into_owned());
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), seen)
}

fn http_resp(status: &str, ct: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[derive(Debug)]
struct Creds;

#[async_trait]
impl CredentialResolver for Creds {
    async fn resolve(
        &self,
        kind: &str,
        _r: &CredentialRef,
    ) -> Result<Map<String, Value>, FlowError> {
        let v = match kind {
            "httpBasicAuth" => json!({"user": "ada", "password": "hunter2-SECRET"}),
            "httpBearerAuth" => json!({"token": "tok-SECRET"}),
            "httpQueryAuth" => json!({"name": "key", "value": "q-SECRET"}),
            _ => json!({}),
        };
        Ok(v.as_object().cloned().unwrap_or_default())
    }
}

fn http_exec() -> Executor {
    let cfg = ExecutorConfig {
        hooks: NodeHooks {
            credentials: Some(Arc::new(Creds)),
            ..NodeHooks::default()
        },
        ..ExecutorConfig::default()
    };
    Executor::new(registry()).with_config(cfg)
}

fn cred(kind: &str) -> Value {
    json!({"credentials": {kind: {"id": "1", "name": "c"}}})
}

#[tokio::test(flavor = "current_thread")]
async fn http_get_basic_auth_query_array_response_no_secret_in_data() {
    let (base, seen) = serve(vec![http_resp(
        "200 OK",
        "application/json",
        r#"[{"id":1},{"id":2}]"#,
    )])
    .await;
    let wf = single(
        "httpRequest",
        4.2,
        json!({
            "method": "GET", "url": format!("{base}/things"),
            "authentication": "genericCredentialType", "genericAuthType": "httpBasicAuth",
            "sendQuery": true, "queryParameters": {"parameters": [{"name": "q", "value": "={{ $json.term }}"}]},
            "sendHeaders": true, "headerParameters": {"parameters": [{"name": "X-Trace", "value": "abc"}]},
            "options": {}
        }),
        cred("httpBasicAuth"),
        json!({}),
    );
    let res = http_exec()
        .execute(
            &wf,
            &ExecuteInput::new("Trigger", items(json!([{"term": "a b"}]))),
        )
        .await
        .unwrap();
    assert!(res.error.is_none(), "{:?}", res.error);
    assert_eq!(
        out_json(&res, "N", 0),
        vec![json!({"id": 1}), json!({"id": 2})]
    );
    let req = seen.lock().unwrap()[0].to_lowercase();
    assert!(
        req.starts_with("get /things?q=a+b") || req.starts_with("get /things?q=a%20b"),
        "{req}"
    );
    assert!(req.contains("x-trace: abc"));
    // base64("ada:hunter2-SECRET")
    assert!(
        req.contains("authorization: basic ywrhomh1bnrlcjitu0vdukvu"),
        "{req}"
    );
    let dump = serde_json::to_string(&res.run_data).unwrap();
    assert!(
        !dump.contains("hunter2") && !dump.contains("SECRET") && !dump.contains("YWRh"),
        "{dump}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn http_post_json_body_full_response_and_errors() {
    let (base, seen) = serve(vec![
        http_resp("201 Created", "application/json", r#"{"ok":true}"#),
        http_resp("404 Not Found", "text/plain", "nope"),
        http_resp("404 Not Found", "text/plain", "nope"),
    ])
    .await;
    let post = |extra: Value| {
        single(
            "httpRequest",
            4.2,
            json!({
                "method": "POST", "url": format!("{base}/x"),
                "authentication": "genericCredentialType", "genericAuthType": "httpBearerAuth",
                "sendBody": true, "contentType": "json", "specifyBody": "keypair",
                "bodyParameters": {"parameters": [{"name": "n", "value": "={{ $json.n }}"}, {"name": "lit", "value": "v"}]},
                "options": {"response": {"response": {"fullResponse": true}}}
            }),
            {
                let mut e = cred("httpBearerAuth");
                e.as_object_mut()
                    .unwrap()
                    .extend(extra.as_object().unwrap().clone());
                e
            },
            json!({}),
        )
    };
    let res = http_exec()
        .execute(
            &post(json!({})),
            &ExecuteInput::new("Trigger", items(json!([{"n": 5}]))),
        )
        .await
        .unwrap();
    let o = &out_json(&res, "N", 0)[0];
    assert_eq!(o["statusCode"], 201);
    assert_eq!(o["body"], json!({"ok": true}));
    let raw = seen.lock().unwrap()[0].clone();
    assert!(
        raw.to_lowercase()
            .contains("authorization: bearer tok-secret")
    );
    assert!(
        raw.ends_with(r#"{"lit":"v","n":5}"#) || raw.ends_with(r#"{"n":5,"lit":"v"}"#),
        "{raw}"
    );

    // 404 stops the run and the message holds no secret
    let res = http_exec()
        .execute(
            &post(json!({})),
            &ExecuteInput::new("Trigger", items(json!([{"n": 1}]))),
        )
        .await
        .unwrap();
    let err = res.error.unwrap();
    assert!(err.message.contains("404"), "{err:?}");
    assert!(!err.message.contains("SECRET"));

    // continueRegularOutput: error becomes an item
    let res = http_exec()
        .execute(
            &post(json!({"onError": "continueRegularOutput"})),
            &ExecuteInput::new("Trigger", items(json!([{"n": 1}]))),
        )
        .await
        .unwrap();
    assert!(res.error.is_none());
    assert!(
        out_json(&res, "N", 0)[0]["error"]
            .as_str()
            .unwrap()
            .contains("404")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn http_text_and_file_formats_and_never_error() {
    let (base, _) = serve(vec![
        http_resp("200 OK", "text/plain", "hello"),
        http_resp(
            "500 Internal Server Error",
            "application/json",
            r#"{"e":1}"#,
        ),
        http_resp("200 OK", "application/octet-stream", "bin"),
    ])
    .await;
    let mk = |opts: Value| {
        single(
            "httpRequest",
            4.2,
            json!({"url": format!("{base}/x"), "options": {"response": {"response": opts}}}),
            json!({}),
            json!({}),
        )
    };
    let run = |wf: Workflow| async move {
        Executor::new(registry())
            .execute(&wf, &ExecuteInput::new("Trigger", items(json!([{}]))))
            .await
            .unwrap()
    };
    let res = run(mk(json!({"outputPropertyName": "body"}))).await;
    assert_eq!(out_json(&res, "N", 0), vec![json!({"body": "hello"})]);
    let res = run(mk(json!({"neverError": true, "fullResponse": true}))).await;
    assert_eq!(out_json(&res, "N", 0)[0]["statusCode"], 500);
    let res = run(mk(json!({"responseFormat": "file"}))).await;
    let it = &res.run_data["N"][0].output(0)[0];
    assert_eq!(it.binary.as_ref().unwrap()["data"].data, "Ymlu");
}

#[tokio::test(flavor = "current_thread")]
async fn http_pagination_next_url_and_param() {
    // next URL taken from the response
    let (base, seen) = serve(vec![
        http_resp(
            "200 OK",
            "application/json",
            r#"{"items":[1],"next":"/p2"}"#,
        ),
        http_resp("200 OK", "application/json", r#"{"items":[2],"next":null}"#),
    ])
    .await;
    let wf = single(
        "httpRequest",
        4.2,
        json!({"url": format!("{base}/p1"), "options": {"pagination": {"pagination": {
            "paginationMode": "responseContainsNextURL",
            "nextURL": "={{ $response.body.next }}",
            "paginationCompleteWhen": "other",
            "completeExpression": "={{ $response.body.next === null }}"
        }}}}),
        json!({}),
        json!({}),
    );
    let res = Executor::new(registry())
        .execute(&wf, &ExecuteInput::new("Trigger", items(json!([{}]))))
        .await
        .unwrap();
    assert!(res.error.is_none(), "{:?}", res.error);
    assert_eq!(
        out_json(&res, "N", 0),
        vec![
            json!({"items": [1], "next": "/p2"}),
            json!({"items": [2], "next": null})
        ]
    );
    assert!(seen.lock().unwrap()[1].starts_with("GET /p2"));

    // update a query parameter from $pageCount until an empty page
    let (base, seen) = serve(vec![
        http_resp("200 OK", "application/json", r#"{"d":[1]}"#),
        http_resp("200 OK", "application/json", r#"{"d":[2]}"#),
        http_resp("200 OK", "application/json", r#"[]"#),
    ])
    .await;
    let wf = single(
        "httpRequest",
        4.2,
        json!({"url": format!("{base}/list"), "options": {"pagination": {"pagination": {
            "paginationMode": "updateAParameterInEachRequest",
            "parameters": {"parameters": [{"type": "qs", "name": "page", "value": "={{ $pageCount + 1 }}"}]},
            "paginationCompleteWhen": "responseIsEmpty"
        }}}}),
        json!({}),
        json!({}),
    );
    let res = Executor::new(registry())
        .execute(&wf, &ExecuteInput::new("Trigger", items(json!([{}]))))
        .await
        .unwrap();
    assert!(res.error.is_none(), "{:?}", res.error);
    assert_eq!(
        out_json(&res, "N", 0),
        vec![json!({"d": [1]}), json!({"d": [2]})]
    );
    let s = seen.lock().unwrap();
    assert!(s[1].starts_with("GET /list?page=2"), "{}", s[1]);
    assert!(s[2].starts_with("GET /list?page=3"), "{}", s[2]);
}
