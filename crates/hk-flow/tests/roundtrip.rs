//! n8n export -> import -> export equivalence on hand-written realistic workflows.
//!
//! "Normalized" = JSON value equality (object key order is not significant).

use std::fs;
use std::path::PathBuf;

use hk_flow::{ErrorMode, Item, Workflow};
use serde_json::Value;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/n8n/workflows")
}

fn fixtures() -> Vec<(String, String)> {
    let mut v: Vec<_> = fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

fn load(name: &str) -> Workflow {
    let (_, s) = fixtures().into_iter().find(|(n, _)| n == name).unwrap();
    Workflow::from_json_str(&s).unwrap()
}

#[test]
fn corpus_has_enough_fixtures() {
    assert!(fixtures().len() >= 3);
}

#[test]
fn export_import_export_is_equivalent() {
    for (name, src) in fixtures() {
        let original: Value = serde_json::from_str(&src).unwrap();
        let wf = Workflow::from_json_str(&src).unwrap_or_else(|e| panic!("{name}: {e}"));
        wf.validate().unwrap_or_else(|e| panic!("{name}: {e}"));

        let first = wf.to_json_value().unwrap();
        assert_eq!(
            first, original,
            "{name}: import->export differs from source"
        );

        // text round trip, twice: output is a fixed point
        let text = wf.to_json_string().unwrap();
        let wf2 = Workflow::from_json_str(&text).unwrap();
        assert_eq!(wf2, wf, "{name}: model differs after text round trip");
        assert_eq!(
            wf2.to_json_string().unwrap(),
            text,
            "{name}: export not stable"
        );
    }
}

#[test]
fn webhook_fixture_typed_view() {
    let wf = load("webhook_respond.json");
    assert_eq!(wf.nodes.len(), 4);
    let hook = wf.node("Webhook").unwrap();
    assert_eq!(hook.key().to_string(), "n8n-nodes-base.webhook@2");
    assert_eq!(
        hook.webhook_id.as_deref(),
        Some("b7a3e0f0-5f1c-4b5a-9a43-3e6f7c1d9e10")
    );
    assert_eq!(wf.node("Edit Fields").unwrap().version().get(), 3.4);
    assert_eq!(wf.node("Sticky Note").unwrap().x(), 200.5);
    assert_eq!(
        wf.settings.as_ref().unwrap().execution_order.as_deref(),
        Some("v1")
    );
    // explicit `staticData: null` is preserved but reads as absent
    assert!(wf.static_data.is_none());
    assert!(wf.extra.contains_key("staticData"));
    let pinned = wf.pinned_items("Webhook").unwrap();
    assert_eq!(pinned[0].json["body"]["name"], "Ada");
}

#[test]
fn if_switch_fixture_typed_view() {
    let wf = load("if_switch.json");
    let fetch = wf.node("Fetch orders").unwrap();
    assert_eq!(fetch.retry_on_fail, Some(true));
    assert_eq!(fetch.max_tries, Some(3));
    assert_eq!(fetch.error_mode(), ErrorMode::ContinueErrorOutput);
    assert_eq!(
        fetch.credentials.as_ref().unwrap()["httpHeaderAuth"]
            .id
            .as_deref(),
        Some("cred-42")
    );
    assert!(wf.node("Small order").unwrap().is_disabled());
    assert_eq!(
        wf.node("Merge").unwrap().error_mode(),
        ErrorMode::ContinueRegularOutput
    );
    // output 1 of Switch feeds Merge input 1; output 2 is a null slot
    assert_eq!(wf.destinations("By region", "main", 1)[0].index, 1);
    assert!(wf.destinations("By region", "main", 2).is_empty());
    assert_eq!(wf.incoming("Merge").len(), 3);
    assert_eq!(wf.settings.as_ref().unwrap().execution_timeout, Some(3600));
    assert_eq!(wf.static_data.as_ref().unwrap()["global"]["lastOrderId"], 2);
    assert!(
        wf.settings
            .as_ref()
            .unwrap()
            .extra
            .contains_key("someFutureSetting")
    );
    assert!(
        wf.node("Merge")
            .unwrap()
            .extra
            .contains_key("hokkabazFutureNodeField")
    );
}

#[test]
fn ai_fixture_typed_view() {
    let wf = load("ai_agent.json");
    let agent_inputs = wf.incoming("AI Agent");
    let types: Vec<_> = agent_inputs.iter().map(|e| e.conn_type).collect();
    assert_eq!(types.len(), 4);
    for t in ["main", "ai_languageModel", "ai_memory", "ai_tool"] {
        assert!(types.contains(&t), "missing {t}");
    }
    // both pinData shapes normalize to items
    let chat = wf.pinned_items("When chat message received").unwrap();
    assert_eq!(chat[0].json["chatInput"], "Where is order 77?");
    let out: &Item = &wf.pinned_items("AI Agent").unwrap()[0];
    assert!(out.paired_item.is_some());
    assert_eq!(wf.hokkabaz_meta().unwrap()["agent"]["mode"], "build");
}

#[test]
fn unsupported_node_imports_intact() {
    let wf = load("unsupported_community.json");
    let n = wf.node("Community thing").unwrap();
    assert_eq!(n.version().get(), 7.25);
    // mistyped / future values are kept verbatim, not coerced
    assert_eq!(n.max_tries, None);
    assert_eq!(n.extra["maxTries"], "5");
    assert_eq!(n.on_error, Some(ErrorMode::Other("someFutureMode".into())));
    assert!(
        n.credentials.as_ref().unwrap()["someCommunityApi"]
            .id
            .is_none()
    );
}

#[test]
fn set_hokkabaz_meta_survives_export() {
    let mut wf = load("webhook_respond.json");
    wf.set_hokkabaz_meta(serde_json::json!({"origin": "test"}));
    let back = Workflow::from_json_value(wf.to_json_value().unwrap()).unwrap();
    assert_eq!(back.hokkabaz_meta().unwrap()["origin"], "test");
    // n8n's own meta keys are untouched
    assert_eq!(
        back.meta.as_ref().unwrap()["templateCredsSetupCompleted"],
        true
    );
}
