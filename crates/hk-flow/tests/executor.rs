//! Executor behavior: ordering, multi in/out, error branches, pinData, retry, disabled
//! nodes, static data, run store. Test nodes (NoOp, Set-lite, ...) live here only; real
//! core nodes are P4.3c.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use hk_flow::testing::{Execution, FlowRunner, RunInput};
use hk_flow::{
    ExecuteInput, Executor, ExecutorConfig, FlowError, FlowNode, InMemoryRunStore, Item,
    NodeContext, NodeOutput, NodeRegistry, NodeTypeDescription, RunStatus, RunStore, Workflow,
    run_data::{GLOBAL_SCOPE, node_scope},
};
use serde_json::{Value, json};

// ------------------------------------------------------------------ test nodes

fn desc(name: &str, outputs: usize) -> NodeTypeDescription {
    serde_json::from_value(json!({
        "displayName": name, "name": format!("test.{name}"), "version": 1,
        "outputs": vec!["main"; outputs],
    }))
    .unwrap()
}

#[derive(Debug)]
struct NoOp(NodeTypeDescription);
#[async_trait]
impl FlowNode for NoOp {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        Ok(vec![ctx.input.first().cloned().unwrap_or_default()])
    }
}

/// Merges `parameters.set` into every item json.
#[derive(Debug)]
struct SetLite(NodeTypeDescription);
#[async_trait]
impl FlowNode for SetLite {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let set = ctx.node.parameters.get("set").and_then(Value::as_object);
        let mut items = ctx.input.first().cloned().unwrap_or_default();
        for it in &mut items {
            for (k, v) in set.into_iter().flatten() {
                it.json.insert(k.clone(), v.clone());
            }
        }
        Ok(vec![items])
    }
}

/// Output 0 = items whose `field` is truthy, output 1 = the rest.
#[derive(Debug)]
struct IfLite(NodeTypeDescription);
#[async_trait]
impl FlowNode for IfLite {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let field = ctx
            .node
            .parameters
            .get("field")
            .and_then(Value::as_str)
            .unwrap_or("ok");
        let (t, f): (Vec<_>, Vec<_>) = ctx.input[0]
            .iter()
            .cloned()
            .partition(|i| i.json.get(field).and_then(Value::as_bool) == Some(true));
        Ok(vec![t, f])
    }
}

/// Concatenates all input ports.
#[derive(Debug)]
struct MergeLite(NodeTypeDescription);
#[async_trait]
impl FlowNode for MergeLite {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        Ok(vec![ctx.input.iter().flatten().cloned().collect()])
    }
}

/// Drops everything.
#[derive(Debug)]
struct Sink(NodeTypeDescription);
#[async_trait]
impl FlowNode for Sink {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, _: &NodeContext) -> Result<NodeOutput, FlowError> {
        Ok(vec![vec![]])
    }
}

/// Fails the first `parameters.failures` attempts, then passes input through.
#[derive(Debug)]
struct Flaky(NodeTypeDescription, Arc<AtomicUsize>);
#[async_trait]
impl FlowNode for Flaky {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let n = self.1.fetch_add(1, Ordering::SeqCst);
        let failures = ctx
            .node
            .parameters
            .get("failures")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX) as usize;
        if n < failures {
            return Err(FlowError::Node {
                node: ctx.node.name.clone(),
                message: format!("boom {n}"),
            });
        }
        Ok(vec![ctx.input[0].clone()])
    }
}

/// Returns input; items with `bad: true` carry the per-item error marker.
#[derive(Debug)]
struct ItemErrors(NodeTypeDescription);
#[async_trait]
impl FlowNode for ItemErrors {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let mut items = ctx.input[0].clone();
        for it in &mut items {
            if it.json.get("bad") == Some(&json!(true)) {
                it.error = Some(json!({"message": "bad item"}));
            }
        }
        Ok(vec![items])
    }
}

/// Counts executions in global and node static data.
#[derive(Debug)]
struct Counter(NodeTypeDescription);
#[async_trait]
impl FlowNode for Counter {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let bump = |v: &mut Value| {
            let n = v["n"].as_u64().unwrap_or(0) + 1;
            v["n"] = json!(n);
            n
        };
        let g = ctx.static_data.update(GLOBAL_SCOPE, bump);
        ctx.static_data.update(&node_scope(&ctx.node.name), bump);
        Ok(vec![vec![Item::from_json(json!({"n": g}))]])
    }
}

fn registry() -> (Arc<NodeRegistry>, Arc<AtomicUsize>) {
    let flaky_calls = Arc::new(AtomicUsize::new(0));
    let mut r = NodeRegistry::new();
    r.register(Arc::new(NoOp(desc("noOp", 1)))).unwrap();
    r.register(Arc::new(SetLite(desc("set", 1)))).unwrap();
    r.register(Arc::new(IfLite(desc("if", 2)))).unwrap();
    r.register(Arc::new(MergeLite(desc("merge", 1)))).unwrap();
    r.register(Arc::new(Sink(desc("sink", 1)))).unwrap();
    r.register(Arc::new(Flaky(desc("flaky", 1), flaky_calls.clone())))
        .unwrap();
    r.register(Arc::new(ItemErrors(desc("itemErrors", 1))))
        .unwrap();
    r.register(Arc::new(Counter(desc("counter", 1)))).unwrap();
    (Arc::new(r), flaky_calls)
}

// --------------------------------------------------------------------- helpers

/// `nodes`: (name, short type, x, y, extra node fields); `edges`: (from, out, to, in).
fn wf(
    order: &str,
    nodes: &[(&str, &str, i64, i64, Value)],
    edges: &[(&str, usize, &str, usize)],
) -> Workflow {
    let nodes: Vec<Value> = nodes
        .iter()
        .map(|(name, ty, x, y, extra)| {
            let mut n = json!({
                "name": name, "type": format!("test.{ty}"), "typeVersion": 1,
                "position": [x, y], "parameters": {},
            });
            for (k, v) in extra.as_object().into_iter().flatten() {
                n[k] = v.clone();
            }
            n
        })
        .collect();
    let mut connections = serde_json::Map::new();
    for (from, out, to, inp) in edges {
        let slots = connections
            .entry(from.to_string())
            .or_insert_with(|| json!({"main": []}))["main"]
            .as_array_mut()
            .unwrap();
        while slots.len() <= *out {
            slots.push(json!([]));
        }
        slots[*out]
            .as_array_mut()
            .unwrap()
            .push(json!({"node": to, "type": "main", "index": inp}));
    }
    Workflow::from_json_value(json!({
        "name": "t", "nodes": nodes, "connections": connections,
        "settings": {"executionOrder": order},
    }))
    .unwrap()
}

fn items(vs: &[Value]) -> Vec<Item> {
    vs.iter().cloned().map(Item::from_json).collect()
}

fn n<'a>(name: &'a str, ty: &'a str, x: i64, y: i64) -> (&'a str, &'a str, i64, i64, Value) {
    (name, ty, x, y, json!({}))
}

async fn run(w: &Workflow, trigger: Vec<Item>) -> hk_flow::ExecutionResult {
    let (reg, _) = registry();
    Executor::new(reg)
        .execute(w, &ExecuteInput::new("T", trigger))
        .await
        .unwrap()
}

/// Node names in the order their runs started.
fn order(res: &hk_flow::ExecutionResult) -> Vec<String> {
    let mut v: Vec<(usize, &String)> = res
        .run_data
        .iter()
        .flat_map(|(k, runs)| runs.iter().map(move |r| (r.execution_index, k)))
        .collect();
    v.sort();
    v.into_iter().map(|(_, k)| k.clone()).collect()
}

fn out(res: &hk_flow::ExecutionResult, node: &str, run: usize, port: usize) -> Vec<Value> {
    res.run_data[node][run]
        .output(port)
        .iter()
        .map(|i| Value::Object(i.json.clone()))
        .collect()
}

// ----------------------------------------------------------------------- tests

#[tokio::test]
async fn linear_chain_records_runs_and_source() {
    let w = wf(
        "v1",
        &[
            n("T", "noOp", 0, 0),
            n("S", "set", 1, 0),
            n("N", "noOp", 2, 0),
        ],
        &[("T", 0, "S", 0), ("S", 0, "N", 0)],
    );
    let mut w = w;
    w.node_mut("S").unwrap().parameters = json!({"set": {"b": 2}}).as_object().unwrap().clone();
    let res = run(&w, items(&[json!({"a": 1}), json!({"a": 2})])).await;
    assert_eq!(res.status, RunStatus::Success);
    assert_eq!(order(&res), ["T", "S", "N"]);
    assert_eq!(
        out(&res, "N", 0, 0),
        [json!({"a": 1, "b": 2}), json!({"a": 2, "b": 2})]
    );
    let src = res.run_data["N"][0].source[0].as_ref().unwrap();
    assert_eq!(src.previous_node, "S");
    // auto pairedItem: equal lengths pair by index
    assert_eq!(
        res.run_data["S"][0].output(0)[1].paired_item,
        Some(hk_flow::PairedItem::Index(1))
    );
}

fn fan_workflow(order: &str) -> Workflow {
    // T fans out to A (y=300), B (y=100), C (y=200), D (y=100, further right than B).
    // Each has one child named <X>2.
    wf(
        order,
        &[
            n("T", "noOp", 0, 0),
            n("A", "noOp", 100, 300),
            n("B", "noOp", 100, 100),
            n("C", "noOp", 100, 200),
            n("D", "noOp", 200, 100),
            n("A2", "noOp", 300, 300),
            n("B2", "noOp", 300, 100),
            n("C2", "noOp", 300, 200),
            n("D2", "noOp", 300, 150),
        ],
        // connection order A, B, C, D deliberately differs from position order
        &[
            ("T", 0, "A", 0),
            ("T", 0, "B", 0),
            ("T", 0, "C", 0),
            ("T", 0, "D", 0),
            ("A", 0, "A2", 0),
            ("B", 0, "B2", 0),
            ("C", 0, "C2", 0),
            ("D", 0, "D2", 0),
        ],
    )
}

#[tokio::test]
async fn v1_runs_branches_depth_first_by_position() {
    let res = run(&fan_workflow("v1"), items(&[json!({})])).await;
    // y asc, x asc on ties; each branch finishes before the next starts
    assert_eq!(
        order(&res),
        ["T", "B", "B2", "D", "D2", "C", "C2", "A", "A2"]
    );
}

#[tokio::test]
async fn v0_runs_breadth_first_in_connection_order() {
    let res = run(&fan_workflow("v0"), items(&[json!({})])).await;
    assert_eq!(
        order(&res),
        ["T", "A", "B", "C", "D", "A2", "B2", "C2", "D2"]
    );
}

#[tokio::test]
async fn multi_output_routes_and_skips_empty_branch() {
    let mut w = wf(
        "v1",
        &[
            n("T", "noOp", 0, 0),
            n("If", "if", 1, 0),
            n("Yes", "noOp", 2, 0),
            n("No", "noOp", 2, 100),
        ],
        &[("T", 0, "If", 0), ("If", 0, "Yes", 0), ("If", 1, "No", 0)],
    );
    w.node_mut("If").unwrap().parameters = json!({"field": "ok"}).as_object().unwrap().clone();
    let res = run(
        &w,
        items(&[json!({"ok": true, "i": 1}), json!({"ok": true, "i": 2})]),
    )
    .await;
    assert_eq!(out(&res, "If", 0, 0).len(), 2);
    assert_eq!(out(&res, "If", 0, 1).len(), 0);
    assert!(res.run_data.contains_key("Yes"));
    assert!(
        !res.run_data.contains_key("No"),
        "empty branch must not run"
    );

    let res = run(&w, items(&[json!({"ok": true}), json!({"ok": false})])).await;
    assert_eq!(out(&res, "Yes", 0, 0), [json!({"ok": true})]);
    assert_eq!(out(&res, "No", 0, 0), [json!({"ok": false})]);
    // top branch (Yes, y=0) first
    assert_eq!(order(&res), ["T", "If", "Yes", "No"]);
}

#[tokio::test]
async fn merge_waits_for_both_inputs() {
    let w = wf(
        "v1",
        &[
            n("T", "noOp", 0, 0),
            n("If", "if", 1, 0),
            n("L", "set", 2, 0),
            n("R", "set", 2, 100),
            n("M", "merge", 3, 0),
        ],
        &[
            ("T", 0, "If", 0),
            ("If", 0, "L", 0),
            ("If", 1, "R", 0),
            ("L", 0, "M", 0),
            ("R", 0, "M", 1),
        ],
    );
    let res = run(
        &w,
        items(&[json!({"ok": true, "v": 1}), json!({"ok": false, "v": 2})]),
    )
    .await;
    assert_eq!(order(&res), ["T", "If", "L", "R", "M"]);
    assert_eq!(res.run_data["M"].len(), 1);
    assert_eq!(
        out(&res, "M", 0, 0),
        [json!({"ok": true, "v": 1}), json!({"ok": false, "v": 2})]
    );
    let m = &res.run_data["M"][0];
    assert_eq!(m.source[0].as_ref().unwrap().previous_node, "L");
    assert_eq!(m.source[1].as_ref().unwrap().previous_node, "R");
}

#[tokio::test]
async fn merge_runs_with_partial_inputs_when_other_branch_is_empty() {
    let w = wf(
        "v1",
        &[
            n("T", "noOp", 0, 0),
            n("If", "if", 1, 0),
            n("L", "noOp", 2, 0),
            n("R", "noOp", 2, 100),
            n("M", "merge", 3, 0),
        ],
        &[
            ("T", 0, "If", 0),
            ("If", 0, "L", 0),
            ("If", 1, "R", 0),
            ("L", 0, "M", 0),
            ("R", 0, "M", 1),
        ],
    );
    let res = run(&w, items(&[json!({"ok": true})])).await;
    assert!(!res.run_data.contains_key("R"));
    assert_eq!(out(&res, "M", 0, 0), [json!({"ok": true})]);
}

#[tokio::test]
async fn two_wires_into_one_port_run_twice() {
    let w = wf(
        "v1",
        &[
            n("T", "noOp", 0, 0),
            n("A", "noOp", 1, 0),
            n("B", "noOp", 1, 100),
            n("X", "noOp", 2, 0),
        ],
        &[
            ("T", 0, "A", 0),
            ("T", 0, "B", 0),
            ("A", 0, "X", 0),
            ("B", 0, "X", 0),
        ],
    );
    let res = run(&w, items(&[json!({})])).await;
    assert_eq!(res.run_data["X"].len(), 2);
    assert_eq!(
        res.run_data["X"][1].source[0]
            .as_ref()
            .unwrap()
            .previous_node,
        "B"
    );
}

#[tokio::test]
async fn execute_once_uses_first_item_only() {
    let mut w = wf(
        "v1",
        &[n("T", "noOp", 0, 0), n("S", "noOp", 1, 0)],
        &[("T", 0, "S", 0)],
    );
    w.node_mut("S").unwrap().execute_once = Some(true);
    let res = run(
        &w,
        items(&[json!({"i": 1}), json!({"i": 2}), json!({"i": 3})]),
    )
    .await;
    assert_eq!(out(&res, "S", 0, 0), [json!({"i": 1})]);
}

#[tokio::test]
async fn always_output_data_keeps_flow_alive() {
    let mut w = wf(
        "v1",
        &[
            n("T", "noOp", 0, 0),
            n("K", "sink", 1, 0),
            n("After", "noOp", 2, 0),
        ],
        &[("T", 0, "K", 0), ("K", 0, "After", 0)],
    );
    let res = run(&w, items(&[json!({"a": 1})])).await;
    assert!(!res.run_data.contains_key("After"));
    w.node_mut("K").unwrap().always_output_data = Some(true);
    let res = run(&w, items(&[json!({"a": 1})])).await;
    assert_eq!(out(&res, "K", 0, 0), [json!({})]);
    assert_eq!(out(&res, "After", 0, 0), [json!({})]);
}

#[tokio::test]
async fn disabled_node_passes_input_through_without_run_entry() {
    let mut w = wf(
        "v1",
        &[
            n("T", "noOp", 0, 0),
            n("S", "set", 1, 0),
            n("N", "noOp", 2, 0),
        ],
        &[("T", 0, "S", 0), ("S", 0, "N", 0)],
    );
    w.node_mut("S").unwrap().disabled = Some(true);
    w.node_mut("S").unwrap().parameters = json!({"set": {"x": 1}}).as_object().unwrap().clone();
    let res = run(&w, items(&[json!({"k": 1}), json!({"k": 2})])).await;
    assert!(!res.run_data.contains_key("S"));
    assert_eq!(out(&res, "N", 0, 0), [json!({"k": 1}), json!({"k": 2})]);
}

fn flaky_wf(extra: Value) -> Workflow {
    let mut w = wf(
        "v1",
        &[
            n("T", "noOp", 0, 0),
            ("F", "flaky", 1, 0, extra),
            n("After", "noOp", 2, 0),
        ],
        &[("T", 0, "F", 0), ("F", 0, "After", 0)],
    );
    w.node_mut("F").unwrap().parameters = json!({"failures": 2}).as_object().unwrap().clone();
    w
}

#[tokio::test(start_paused = true)]
async fn retry_on_fail_succeeds_within_max_tries() {
    let w = flaky_wf(json!({"retryOnFail": true, "maxTries": 3, "waitBetweenTries": 100}));
    let (reg, calls) = registry();
    let t0 = tokio::time::Instant::now();
    let res = Executor::new(reg)
        .execute(&w, &ExecuteInput::new("T", items(&[json!({"a": 1})])))
        .await
        .unwrap();
    assert_eq!(res.status, RunStatus::Success);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(t0.elapsed(), std::time::Duration::from_millis(200));
    assert_eq!(res.run_data["F"].len(), 1, "retries are one run");
    assert!(res.run_data.contains_key("After"));
}

#[tokio::test(start_paused = true)]
async fn retry_exhausted_fails_execution() {
    let w = flaky_wf(json!({"retryOnFail": true, "maxTries": 2, "waitBetweenTries": 10}));
    let (reg, calls) = registry();
    let res = Executor::new(reg)
        .execute(&w, &ExecuteInput::new("T", items(&[json!({})])))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(res.status, RunStatus::Error);
    assert_eq!(res.error.as_ref().unwrap().node.as_deref(), Some("F"));
    assert_eq!(res.error.as_ref().unwrap().message, "boom 1");
    assert!(!res.run_data.contains_key("After"));
}

#[tokio::test]
async fn no_retry_by_default() {
    let w = flaky_wf(json!({}));
    let (reg, calls) = registry();
    let res = Executor::new(reg)
        .execute(&w, &ExecuteInput::new("T", items(&[json!({})])))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(res.status, RunStatus::Error);
}

#[tokio::test]
async fn on_error_stop_workflow_records_error_run() {
    let w = flaky_wf(json!({"onError": "stopWorkflow"}));
    let res = run(&w, items(&[json!({})])).await;
    assert_eq!(res.status, RunStatus::Error);
    let r = &res.run_data["F"][0];
    assert_eq!(r.execution_status, RunStatus::Error);
    assert!(r.error.is_some());
    assert_eq!(res.last_node_executed.as_deref(), Some("F"));
    assert!(!res.run_data.contains_key("After"));
}

#[tokio::test]
async fn on_error_continue_regular_output_emits_error_items() {
    let w = flaky_wf(json!({"onError": "continueRegularOutput"}));
    let res = run(&w, items(&[json!({"i": 1}), json!({"i": 2})])).await;
    assert_eq!(res.status, RunStatus::Success);
    assert_eq!(
        out(&res, "After", 0, 0),
        [json!({"error": "boom 0"}), json!({"error": "boom 0"})]
    );
    // legacy flag behaves the same
    let w = flaky_wf(json!({"continueOnFail": true}));
    let res = run(&w, items(&[json!({"i": 1})])).await;
    assert_eq!(out(&res, "After", 0, 0), [json!({"error": "boom 0"})]);
}

#[tokio::test]
async fn on_error_continue_error_output_routes_to_extra_output() {
    let mut w = wf(
        "v1",
        &[
            n("T", "noOp", 0, 0),
            (
                "F",
                "flaky",
                1,
                0,
                json!({"onError": "continueErrorOutput"}),
            ),
            n("Ok", "noOp", 2, 0),
            n("Err", "noOp", 2, 100),
        ],
        &[("T", 0, "F", 0), ("F", 0, "Ok", 0), ("F", 1, "Err", 0)],
    );
    w.node_mut("F").unwrap().parameters = json!({"failures": 1}).as_object().unwrap().clone();
    let res = run(&w, items(&[json!({"i": 1})])).await;
    assert_eq!(res.status, RunStatus::Success);
    assert!(!res.run_data.contains_key("Ok"));
    assert_eq!(out(&res, "Err", 0, 0), [json!({"i": 1, "error": "boom 0"})]);
    assert_eq!(res.run_data["F"][0].data.main.len(), 2);
}

#[tokio::test]
async fn on_error_continue_error_output_splits_per_item_errors() {
    let w = wf(
        "v1",
        &[
            n("T", "noOp", 0, 0),
            (
                "E",
                "itemErrors",
                1,
                0,
                json!({"onError": "continueErrorOutput"}),
            ),
            n("Ok", "noOp", 2, 0),
            n("Err", "noOp", 2, 100),
        ],
        &[("T", 0, "E", 0), ("E", 0, "Ok", 0), ("E", 1, "Err", 0)],
    );
    let res = run(
        &w,
        items(&[
            json!({"i": 1}),
            json!({"i": 2, "bad": true}),
            json!({"i": 3}),
        ]),
    )
    .await;
    assert_eq!(out(&res, "Ok", 0, 0), [json!({"i": 1}), json!({"i": 3})]);
    assert_eq!(out(&res, "Err", 0, 0), [json!({"i": 2, "bad": true})]);
}

fn pinned_wf() -> Workflow {
    let mut w = wf(
        "v1",
        &[
            n("T", "noOp", 0, 0),
            ("F", "flaky", 1, 0, json!({})),
            n("After", "noOp", 2, 0),
        ],
        &[("T", 0, "F", 0), ("F", 0, "After", 0)],
    );
    // F would always fail; pin makes it succeed. Mixed pin shapes on purpose.
    w.pin_data = Some(
        serde_json::from_value(json!({
            "F": [{"json": {"pinned": 1}}, {"pinned": 2}],
            "T": [{"json": {"from": "pin"}}],
        }))
        .unwrap(),
    );
    w
}

#[tokio::test]
async fn pin_data_skips_execution_when_enabled() {
    let (reg, calls) = registry();
    let ex = Executor::new(reg).with_config(ExecutorConfig {
        use_pin_data: true,
        ..ExecutorConfig::default()
    });
    let res = ex
        .execute(
            &pinned_wf(),
            &ExecuteInput::new("T", items(&[json!({"from": "trigger"})])),
        )
        .await
        .unwrap();
    assert_eq!(res.status, RunStatus::Success);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "pinned node must not execute"
    );
    assert_eq!(
        out(&res, "T", 0, 0),
        [json!({"from": "pin"})],
        "pinned trigger replaces payload"
    );
    assert_eq!(
        out(&res, "After", 0, 0),
        [json!({"pinned": 1}), json!({"pinned": 2})]
    );
}

#[tokio::test]
async fn pin_data_ignored_by_default() {
    let (reg, calls) = registry();
    let res = Executor::new(reg)
        .execute(
            &pinned_wf(),
            &ExecuteInput::new("T", items(&[json!({"from": "trigger"})])),
        )
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(res.status, RunStatus::Error);
    assert_eq!(out(&res, "T", 0, 0), [json!({"from": "trigger"})]);
}

#[tokio::test]
async fn static_data_round_trips_between_executions() {
    let (reg, _) = registry();
    let ex = Executor::new(reg);
    let mut w = wf(
        "v1",
        &[n("T", "noOp", 0, 0), n("C", "counter", 1, 0)],
        &[("T", 0, "C", 0)],
    );
    let r1 = ex
        .execute(&w, &ExecuteInput::new("T", items(&[json!({})])))
        .await
        .unwrap();
    assert_eq!(out(&r1, "C", 0, 0), [json!({"n": 1})]);
    let sd = r1.static_data.unwrap();
    assert_eq!(sd, json!({"global": {"n": 1}, "node:C": {"n": 1}}));

    w.static_data = Some(sd);
    let r2 = ex
        .execute(&w, &ExecuteInput::new("T", items(&[json!({})])))
        .await
        .unwrap();
    assert_eq!(out(&r2, "C", 0, 0), [json!({"n": 2})]);

    // untouched static data is not reported
    let w2 = wf(
        "v1",
        &[n("T", "noOp", 0, 0), n("N", "noOp", 1, 0)],
        &[("T", 0, "N", 0)],
    );
    let r3 = ex
        .execute(&w2, &ExecuteInput::new("T", items(&[json!({})])))
        .await
        .unwrap();
    assert!(r3.static_data.is_none());
}

#[tokio::test]
async fn run_store_gets_checkpoints_and_final_record() {
    let (reg, _) = registry();
    let store = Arc::new(InMemoryRunStore::new());
    let ex = Executor::new(reg).with_store(store.clone());
    let w = wf(
        "v1",
        &[n("T", "noOp", 0, 0), n("N", "noOp", 1, 0)],
        &[("T", 0, "N", 0)],
    );
    let mut input = ExecuteInput::new("T", items(&[json!({"a": 1})]));
    input.execution_id = Some("e1".into());
    let res = ex.execute(&w, &input).await.unwrap();

    let rec = store.load("e1").await.unwrap().unwrap();
    assert_eq!(rec.status, RunStatus::Success);
    assert!(rec.finished_at.is_some());
    assert_eq!(rec.run_data, res.run_data);
    assert_eq!(rec.last_node_executed.as_deref(), Some("N"));
    assert_eq!(store.list(None).await.unwrap().len(), 1);
    assert!(store.delete("e1").await.unwrap());
    assert!(store.load("e1").await.unwrap().is_none());
}

#[tokio::test]
async fn unsupported_node_type_fails_execution_not_import() {
    let w = wf(
        "v1",
        &[n("T", "noOp", 0, 0), n("X", "doesNotExist", 1, 0)],
        &[("T", 0, "X", 0)],
    );
    let res = run(&w, items(&[json!({})])).await;
    assert_eq!(res.status, RunStatus::Error);
    assert!(res.error.unwrap().message.contains("not supported"));
}

#[tokio::test]
async fn loop_guard_stops_cycles() {
    let w = wf(
        "v1",
        &[
            n("T", "noOp", 0, 0),
            n("A", "noOp", 1, 0),
            n("B", "noOp", 2, 0),
        ],
        &[("T", 0, "A", 0), ("A", 0, "B", 0), ("B", 0, "A", 0)],
    );
    let (reg, _) = registry();
    let ex = Executor::new(reg).with_config(ExecutorConfig {
        max_node_runs: 10,
        ..ExecutorConfig::default()
    });
    let res = ex
        .execute(&w, &ExecuteInput::new("T", items(&[json!({})])))
        .await
        .unwrap();
    assert_eq!(res.status, RunStatus::Error);
    assert!(res.error.unwrap().message.contains("loop guard"));
}

#[tokio::test]
async fn execution_data_has_no_parameters_or_credentials() {
    let mut w = wf(
        "v1",
        &[n("T", "noOp", 0, 0), n("N", "noOp", 1, 0)],
        &[("T", 0, "N", 0)],
    );
    w.node_mut("N").unwrap().parameters = json!({"apiKey": "s3cr3t-value"})
        .as_object()
        .unwrap()
        .clone();
    let res = run(&w, items(&[json!({})])).await;
    let dump = serde_json::to_string(&res.run_data).unwrap();
    assert!(!dump.contains("s3cr3t-value"));
}

#[tokio::test]
async fn runs_as_corpus_flow_runner() {
    let (reg, _) = registry();
    let ex = Executor::new(reg);
    let w = wf(
        "v1",
        &[n("T", "noOp", 0, 0), n("N", "noOp", 1, 0)],
        &[("T", 0, "N", 0)],
    );
    let trig = items(&[json!({"a": 1})]);
    let exec = ex
        .run(RunInput {
            workflow: &w,
            trigger_node: "T",
            trigger_items: &trig,
        })
        .await;
    let Execution::Done(rd) = exec else {
        panic!("expected Done")
    };
    assert_eq!(
        rd["N"][0].data.main[0].as_ref().unwrap()[0].json["a"],
        json!(1)
    );
}
