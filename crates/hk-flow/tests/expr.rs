//! Expression corpus: tests/expr/cases/*.json, each an array of
//! `{ name, expression, context, expected | error, limits? }` (n8n docs examples).
//!
//! Run subset: `cargo test -p hk-flow --test expr -- <name-substring>`; set
//! `HK_EXPR_CASE=<substring>` to filter the corpus run.
//!
//! `context` keys: `json` (shorthand for one input item) or `input` (items per port),
//! `item_index`, `run_index`, `node`, `parameters`, `source`, `nodes`
//! (`{name: {parameters, runs: [NodeRun]}}`), `workflow`, `execution_id`, `mode`,
//! `env`, `vars`, `timezone`, `now` (unix ms; defaults to 2024-01-15T10:30:45.123Z).
//! `expected` is compared as JSON; `error` is a substring of the failure message.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hk_flow::expr::shared_run_data;
use hk_flow::{ExprLimits, Item, Node, NodeContext, QuickJsEvaluator, RunData, RunSource};
use serde::Deserialize;
use serde_json::{Map, Value, json};

const NOW_MS: i64 = 1_705_314_645_123;

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    expression: Value,
    #[serde(default)]
    context: Map<String, Value>,
    #[serde(default)]
    expected: Value,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    limits: Option<Limits>,
}

#[derive(Debug, Deserialize)]
struct Limits {
    timeout_ms: Option<u64>,
    memory_mb: Option<usize>,
}

fn items(v: &Value) -> Vec<Item> {
    serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("bad items {v}: {e}"))
}

fn build_ctx(c: &Map<String, Value>, evaluator: Arc<QuickJsEvaluator>) -> (NodeContext, usize) {
    let mut node = Node::new(
        c.get("node").and_then(Value::as_str).unwrap_or("Current"),
        "n8n-nodes-base.set",
        1.into(),
        [0.into(), 0.into()],
    );
    if let Some(Value::Object(p)) = c.get("parameters") {
        node.parameters = p.clone();
    }
    let input: Vec<Vec<Item>> = if let Some(i) = c.get("input") {
        i.as_array().unwrap().iter().map(items).collect()
    } else if let Some(j) = c.get("json") {
        vec![vec![Item::from_json(j.clone())]]
    } else {
        vec![vec![]]
    };
    let item_index = c.get("item_index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let run_index = c.get("run_index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let mut ctx = NodeContext::new(node, input, run_index);
    if let Some(s) = c.get("source") {
        ctx.source = serde_json::from_value::<Vec<Option<RunSource>>>(s.clone()).unwrap();
    }
    let mut run_data = RunData::new();
    let mut params = std::collections::BTreeMap::new();
    if let Some(Value::Object(nodes)) = c.get("nodes") {
        for (name, n) in nodes {
            params.insert(
                name.clone(),
                n.get("parameters").cloned().unwrap_or(json!({})),
            );
            let runs = n.get("runs").cloned().unwrap_or(json!([]));
            run_data.insert(name.clone(), serde_json::from_value(runs).unwrap());
        }
    }
    ctx.run_data = shared_run_data(run_data);
    ctx.expr.evaluator = Some(evaluator);
    ctx.expr.node_params = Arc::new(params);
    ctx.expr.now_ms = Some(c.get("now").and_then(Value::as_i64).unwrap_or(NOW_MS));
    ctx.expr.timezone = Some(
        c.get("timezone")
            .and_then(Value::as_str)
            .unwrap_or("UTC")
            .to_owned(),
    );
    if let Some(Value::Object(w)) = c.get("workflow") {
        ctx.workflow_id = w.get("id").and_then(Value::as_str).map(str::to_owned);
        ctx.workflow_name = w.get("name").and_then(Value::as_str).map(str::to_owned);
        ctx.expr.workflow_active = w.get("active").and_then(Value::as_bool).unwrap_or(false);
    }
    ctx.execution_id = c
        .get("execution_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    ctx.expr.mode = c.get("mode").and_then(Value::as_str).map(str::to_owned);
    if let Some(Value::Object(e)) = c.get("env") {
        ctx.expr.env = Arc::new(
            e.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                .collect(),
        );
    }
    if let Some(Value::Object(v)) = c.get("vars") {
        ctx.expr.vars = Arc::new(v.clone());
    }
    (ctx, item_index)
}

fn evaluator_for(case: &Case, shared: &Arc<QuickJsEvaluator>) -> Arc<QuickJsEvaluator> {
    match &case.limits {
        None => Arc::clone(shared),
        Some(l) => {
            let mut lim = ExprLimits::default();
            if let Some(t) = l.timeout_ms {
                lim.timeout = Duration::from_millis(t);
            }
            if let Some(m) = l.memory_mb {
                lim.memory_bytes = m * 1024 * 1024;
            }
            Arc::new(QuickJsEvaluator::new(lim))
        }
    }
}

fn load() -> Vec<(String, Case)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/expr/cases");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let cases: Vec<Case> =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        let stem = f.file_stem().unwrap().to_string_lossy().into_owned();
        out.extend(cases.into_iter().map(|c| (stem.clone(), c)));
    }
    out
}

#[test]
fn expr_corpus() {
    let cases = load();
    assert!(cases.len() >= 40, "corpus shrank: {}", cases.len());
    let filter = std::env::var("HK_EXPR_CASE").ok();
    let shared = Arc::new(QuickJsEvaluator::new(ExprLimits::default()));
    let mut failures = Vec::new();
    let mut ran = 0;
    for (file, case) in &cases {
        if filter
            .as_ref()
            .is_some_and(|f| !case.name.contains(f.as_str()))
        {
            continue;
        }
        ran += 1;
        let ev = evaluator_for(case, &shared);
        let (ctx, idx) = build_ctx(&case.context, ev);
        let got = ctx.eval_param(&case.expression, idx);
        let label = format!("{file}/{}", case.name);
        let want = &case.expected;
        match (&case.error, got) {
            (None, Ok(v)) if &v == want => {}
            (None, Ok(v)) => failures.push(format!("{label}: want {want}, got {v}")),
            (None, Err(e)) => failures.push(format!("{label}: want {want}, err {e}")),
            (Some(sub), Err(e)) if e.to_string().contains(sub.as_str()) => {}
            (Some(sub), Err(e)) => failures.push(format!("{label}: want error ~{sub:?}, got {e}")),
            (Some(sub), Ok(v)) => failures.push(format!("{label}: want error ~{sub:?}, got {v}")),
        }
    }
    assert!(ran > 0, "filter matched no cases");
    assert!(
        failures.is_empty(),
        "\n{}\n{}/{ran} failed",
        failures.join("\n"),
        failures.len()
    );
}

#[test]
fn expr_eval_param_recurses_and_skips_literals() {
    let shared = Arc::new(QuickJsEvaluator::new(ExprLimits::default()));
    let mut ctx_map = Map::new();
    ctx_map.insert("json".into(), json!({"n": 2, "s": "x"}));
    let (ctx, _) = build_ctx(&ctx_map, shared);
    let v = json!({"a": "={{ $json.n * 2 }}", "b": ["=v={{ $json.s }}", "lit", 3, null], "c": {"d": "={{ $json.s }}"}});
    assert_eq!(
        ctx.eval_param(&v, 0).unwrap(),
        json!({"a": 4, "b": ["v=x", "lit", 3, null], "c": {"d": "x"}})
    );
}

#[test]
fn expr_param_path_lookup() {
    let shared = Arc::new(QuickJsEvaluator::new(ExprLimits::default()));
    let mut ctx_map = Map::new();
    ctx_map.insert("json".into(), json!({"n": 2}));
    ctx_map.insert(
        "parameters".into(),
        json!({"options": {"limit": "={{ $json.n + 1 }}"}}),
    );
    let (ctx, _) = build_ctx(&ctx_map, shared);
    assert_eq!(
        ctx.eval_param_at("options.limit", 0).unwrap(),
        Some(json!(3))
    );
    assert_eq!(ctx.eval_param_at("options.nope", 0).unwrap(), None);
}

#[test]
fn expr_per_item_index_and_shared_session() {
    let shared = Arc::new(QuickJsEvaluator::new(ExprLimits::default()));
    let mut ctx_map = Map::new();
    ctx_map.insert(
        "input".into(),
        json!([[{"json": {"v": 1}}, {"json": {"v": 2}}, {"json": {"v": 3}}]]),
    );
    let (ctx, _) = build_ctx(&ctx_map, shared);
    let got: Vec<Value> = (0..3)
        .map(|i| {
            ctx.eval_param(&json!("={{ $json.v * 10 + $itemIndex }}"), i)
                .unwrap()
        })
        .collect();
    assert_eq!(got, [json!(10), json!(21), json!(32)]);
}

#[test]
fn expr_mutating_json_does_not_leak_between_contexts() {
    let shared = Arc::new(QuickJsEvaluator::new(ExprLimits::default()));
    let mut m = Map::new();
    m.insert("json".into(), json!({"a": 1}));
    m.insert("execution_id".into(), json!("e1"));
    let (c1, _) = build_ctx(&m, Arc::clone(&shared));
    c1.eval_param(&json!("={{ $json.a = 99 }}"), 0).unwrap();
    let (c2, _) = build_ctx(&m, shared);
    assert_eq!(
        c2.eval_param(&json!("={{ $json.a }}"), 0).unwrap(),
        json!(1)
    );
}
