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

use hk_flow::testing::{CaseFilter, load_cases, run_corpus};
use hk_flow::{Executor, NodeRegistry, register_core_nodes};

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
    let exec = Executor::new(Arc::new(registry));
    let report = run_corpus(&exec, &cases).await;
    eprintln!("{}", report.render());
    assert!(report.is_ok(), "\n{}", report.render());
}
