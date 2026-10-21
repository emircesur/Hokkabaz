//! n8n compat corpus: tests/n8n/cases/<case>/{workflow,trigger,runData,meta}.json.
//!
//! Run subset: `HK_N8N_NODE=merge cargo test -p hk-flow --test n8n_corpus -- --nocapture`
//! (comma list ok), or `HK_N8N_CASE=<substring>`. Cargo name filter works on the two tests.
//! Swap `PendingRunner` for the P4.2 engine runner to turn pending cases into real checks.

use std::path::PathBuf;

use hk_flow::testing::{CaseFilter, PendingRunner, load_cases, run_corpus};

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
    let report = run_corpus(&PendingRunner, &cases).await;
    eprintln!("{}", report.render());
    assert!(report.is_ok(), "\n{}", report.render());
}
