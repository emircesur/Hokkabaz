//! Code node beyond the corpus: error line info, return-value shapes, sandbox limits.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hk_flow::{
    ExprLimits, FlowError, Item, Node, NodeContext, NodeRegistry, QuickJsEvaluator,
    register_core_nodes,
};
use serde_json::{Value, json};

async fn run_with(
    params: Value,
    input: Value,
    limits: Option<ExprLimits>,
) -> Result<Vec<Vec<Item>>, FlowError> {
    let mut reg = NodeRegistry::new();
    register_core_nodes(&mut reg).unwrap();
    let mut node = Node::new(
        "Code",
        "n8n-nodes-base.code",
        2.into(),
        [0.into(), 0.into()],
    );
    node.parameters = params.as_object().unwrap().clone();
    let items = input
        .as_array()
        .unwrap()
        .iter()
        .map(Item::from_pinned)
        .collect();
    let mut ctx = NodeContext::new(node.clone(), vec![items], 0);
    if let Some(l) = limits {
        ctx.expr.evaluator = Some(Arc::new(QuickJsEvaluator::new(l)));
    }
    reg.resolve(&node).unwrap().execute(&ctx).await
}

fn all(code: &str) -> Value {
    json!({"jsCode": code})
}

fn each(code: &str) -> Value {
    json!({"mode": "runOnceForEachItem", "jsCode": code})
}

fn jsons(out: &[Vec<Item>]) -> Vec<Value> {
    out[0]
        .iter()
        .map(|i| Value::Object(i.json.clone()))
        .collect()
}

fn msg(e: FlowError) -> String {
    match e {
        FlowError::Node { message, .. } => message,
        other => panic!("not a node error: {other}"),
    }
}

#[tokio::test]
async fn all_items_shapes() {
    let input = json!([{"a": 1}, {"a": 2}]);
    // plain objects wrap into json
    let out = run_with(all("return [{x: 1}, {x: 2}]"), input.clone(), None)
        .await
        .unwrap();
    assert_eq!(jsons(&out), [json!({"x": 1}), json!({"x": 2})]);
    // single object, item form with pairedItem
    let out = run_with(
        all("return {json: {x: 1}, pairedItem: 1, junk: 1}"),
        input.clone(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(jsons(&out), [json!({"x": 1})]);
    assert_eq!(out[0][0].paired_item, Some(hk_flow::PairedItem::Index(1)));
    // null = no output
    let out = run_with(all("return null"), input.clone(), None)
        .await
        .unwrap();
    assert!(out[0].is_empty());
    // no return
    let e = run_with(all("const x = 1"), input.clone(), None)
        .await
        .unwrap_err();
    assert!(msg(e).contains("doesn't return items properly"));
    // scalar element
    let e = run_with(all("return [1]"), input.clone(), None)
        .await
        .unwrap_err();
    assert!(msg(e).contains("[for item 0]"));
    // mixed json / plain
    let e = run_with(all("return [{json: {}}, {a: 1}]"), input, None)
        .await
        .unwrap_err();
    assert!(msg(e).contains("all returned items need it"));
}

#[tokio::test]
async fn globals_and_legacy_names() {
    let input = json!([{"n": 1}, {"n": 2}, {"n": 3}]);
    let out = run_with(
        all("return [{ \
            sum: items.reduce((s, i) => s + i.json.n, 0), \
            first: $input.first().json.n, last: $input.last().json.n, \
            len: $input.all().length, items: $items().length, \
            idx: $itemIndex, wf: typeof $workflow, ex: typeof $execution, \
            now: typeof $now.toMillis(), today: $today.hour }]"),
        input.clone(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        jsons(&out),
        [
            json!({"sum": 6, "first": 1, "last": 3, "len": 3, "items": 3, "idx": 0,
                "wf": "object", "ex": "object", "now": "number", "today": 0})
        ]
    );
    // per item: $json, item, $input.item, $itemIndex; null drops; auto pairing
    let out = run_with(
        each("if ($json.n === 2) return null; item.json.k = $itemIndex; return {json: {n: $json.n, k: item.json.k, same: $input.item.json.n === $json.n}}"),
        input,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        jsons(&out),
        [
            json!({"n": 1, "k": 0, "same": true}),
            json!({"n": 3, "k": 2, "same": true})
        ]
    );
    assert_eq!(out[0][1].paired_item, Some(hk_flow::PairedItem::Index(2)));
}

#[tokio::test]
async fn await_works_and_console_is_captured() {
    let out = run_with(
        all("console.log('hi', {a: 1}); const v = await Promise.resolve(5); return [{v}]"),
        json!([{}]),
        None,
    )
    .await
    .unwrap();
    assert_eq!(jsons(&out), [json!({"v": 5})]);
}

#[tokio::test]
async fn errors_carry_line_and_item() {
    let e = run_with(
        all("const a = 1;\nconst b = 2;\nthrow new Error('boom');"),
        json!([{}]),
        None,
    )
    .await
    .unwrap_err();
    assert!(msg(e).contains("boom [line 3]"), "line info");
    let e = run_with(all("return [\n{a: nope}]"), json!([{}]), None)
        .await
        .unwrap_err();
    let m = msg(e);
    assert!(m.contains("nope") && m.contains("[line 2]"), "{m}");
    let e = run_with(
        each("if ($itemIndex === 1) {\n  throw new TypeError('bad');\n}\nreturn {};"),
        json!([{}, {}]),
        None,
    )
    .await
    .unwrap_err();
    assert!(msg(e).contains("bad [line 2, for item 1]"));
    let e = run_with(all("return [;"), json!([{}]), None)
        .await
        .unwrap_err();
    assert!(msg(e).to_lowercase().contains("unexpected"), "syntax error");
}

#[tokio::test]
async fn python_is_off_unless_opted_in() {
    let e = run_with(
        json!({"language": "python", "pythonCode": "return []"}),
        json!([{}]),
        None,
    )
    .await
    .unwrap_err();
    let m = msg(e);
    assert!(
        m.contains("off by default") || m.contains("not available"),
        "{m}"
    );
}

#[tokio::test]
async fn sandbox_has_no_fs_net_process() {
    let out = run_with(
        all("return [{ r: typeof require, p: typeof process, f: typeof fetch, x: typeof XMLHttpRequest, h: typeof __s }]"),
        json!([{}]),
        None,
    )
    .await
    .unwrap();
    let j = &jsons(&out)[0];
    for k in ["r", "p", "f", "x"] {
        assert_eq!(j[k], "undefined", "{k}");
    }
}

#[tokio::test]
async fn infinite_loop_times_out() {
    let limits = ExprLimits {
        timeout: Duration::from_millis(200),
        ..ExprLimits::default()
    };
    let t = Instant::now();
    let e = run_with(all("while (true) {}"), json!([{}]), Some(limits.clone()))
        .await
        .unwrap_err();
    assert!(msg(e).contains("timed out"));
    assert!(t.elapsed() < Duration::from_secs(5));
    // catching must not swallow the interrupt
    let e = run_with(
        all("try { for (;;) {} } catch (e) {} return [{}]"),
        json!([{}]),
        Some(limits.clone()),
    )
    .await
    .unwrap_err();
    assert!(msg(e).contains("timed out"));
    // per-item mode shares one deadline
    let e = run_with(each("while (true) {}"), json!([{}]), Some(limits))
        .await
        .unwrap_err();
    assert!(msg(e).contains("timed out"));
}

#[tokio::test]
async fn memory_cap() {
    let limits = ExprLimits {
        memory_bytes: 8 * 1024 * 1024,
        ..ExprLimits::default()
    };
    let e = run_with(
        all("const a = []; for (;;) a.push('x'.repeat(1024) + a.length); return [{}]"),
        json!([{}]),
        Some(limits.clone()),
    )
    .await
    .unwrap_err();
    let m = msg(e);
    assert!(m.contains("memory"), "{m}");
    // realm is rebuilt afterwards: a fresh evaluator-independent run still works
    let out = run_with(all("return [{ok: 1}]"), json!([{}]), Some(limits))
        .await
        .unwrap();
    assert_eq!(jsons(&out), [json!({"ok": 1})]);
}

#[tokio::test]
async fn stack_cap() {
    let e = run_with(
        all("function f(){ return f() + 1 } return [{v: f()}]"),
        json!([{}]),
        None,
    )
    .await
    .unwrap_err();
    let m = msg(e);
    assert!(m.to_lowercase().contains("stack"), "{m}");
}
