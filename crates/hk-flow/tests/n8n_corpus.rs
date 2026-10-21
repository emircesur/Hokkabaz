//! n8n compat corpus: tests/n8n/cases/<case>/{workflow,trigger,runData,meta}.json.
//!
//! Run subset: `HK_N8N_NODE=merge cargo test -p hk-flow --test n8n_corpus -- --nocapture`
//! (comma list ok), or `HK_N8N_CASE=<substring>`. Cargo name filter works on the two tests.
//! Cases run through the real [`Executor`] with the core nodes registered. A case may stay
//! `pending_executor` only while its node is not implemented (the executor then fails with
//! `UnsupportedNode`, which the harness treats as pending only via `PendingRunner`; keep
//! such cases out of the corpus or implement the node).

use std::path::PathBuf;
use std::sync::Arc;

use hk_flow::nodes::ai::ScriptedChatModelFactory;
use hk_flow::testing::{CaseFilter, load_cases, run_corpus};
use hk_flow::{Executor, ExecutorConfig, NodeHooks, NodeRegistry, register_core_nodes};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/n8n/cases")
}

#[test]
fn fixtures_are_wellformed() {
    let cases = load_cases(&root()).unwrap_or_else(|e| panic!("{e}"));
    assert!(cases.len() >= 20, "seed corpus shrank: {}", cases.len());
    let mut problems = Vec::new();
    for c in &cases {
        for p in c.static_problems() {
            problems.push(format!("{}: {p}", c.name));
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::print_stderr)]
async fn corpus_against_runner() {
    let cases = CaseFilter::from_env().apply(load_cases(&root()).unwrap_or_else(|e| panic!("{e}")));
    let mut registry = NodeRegistry::new();
    register_core_nodes(&mut registry).unwrap_or_else(|e| panic!("{e}"));
    let registry = Arc::new(registry);
    let exec = Executor::new(Arc::clone(&registry));
    // AI cases ship `llm.json`: replies of a scripted chat model, one executor per case
    let (scripted, plain): (Vec<_>, Vec<_>) = cases
        .into_iter()
        .partition(|c| c.dir.join("llm.json").exists());
    let mut report = run_corpus(&exec, &plain).await;
    for case in scripted {
        let script: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(case.dir.join("llm.json")).unwrap_or_else(|e| panic!("{e}")),
        )
        .unwrap_or_else(|e| panic!("{}: llm.json: {e}", case.name));
        let factory = ScriptedChatModelFactory::from_json(&script).unwrap_or_else(|e| panic!("{e}"));
        let exec = Executor::new(Arc::clone(&registry)).with_config(ExecutorConfig {
            hooks: NodeHooks {
                chat_models: Some(Arc::new(factory)),
                ..NodeHooks::default()
            },
            ..ExecutorConfig::default()
        });
        report
            .cases
            .extend(run_corpus(&exec, std::slice::from_ref(&case)).await.cases);
    }
    eprintln!("{}", report.render());
    assert!(report.is_ok(), "\n{}", report.render());
}
