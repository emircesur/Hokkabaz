//! n8n compat corpus harness (feature `testing`).
//!
//! A case is a directory under `tests/n8n/cases/`:
//!
//! ```text
//! <case>/workflow.json   n8n workflow export
//! <case>/trigger.json    items the trigger node emits (array of `{json}` or bare objects)
//! <case>/runData.json    expected n8n runData: { Node: [ { data: { main: [[ {json} ]] } } ] }
//! <case>/meta.json       optional: description, status, nodes, docs, trigger_node
//! ```
//!
//! Expectations are hand-written from the public n8n docs (clean-room), never captured
//! from n8n source. Only `json` of items is compared; pairedItem/binary are not yet.
//! A node absent from `runData.json` is expected NOT to run (e.g. empty branch).
//!
//! Cases marked `pending_executor` pass while the runner reports [`Execution::Pending`]
//! and are checked for real the moment a runner returns [`Execution::Done`].

use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Item, Workflow};

const N8N_BASE_PREFIX: &str = "n8n-nodes-base.";

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("{path}: {message}")]
    Case { path: String, message: String },
}

fn case_err(path: &Path, message: impl std::fmt::Display) -> HarnessError {
    HarnessError::Case {
        path: path.display().to_string(),
        message: message.to_string(),
    }
}

// ------------------------------------------------------------------ fixtures

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    /// Executor must produce a matching runData.
    #[default]
    Active,
    /// Expectation written, executor not there yet (P4.2). Pending run = pass.
    PendingExecutor,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaseMeta {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub status: CaseStatus,
    /// Node types under test (`n8n-nodes-base.set` or short `set`). Empty = all in workflow.
    #[serde(default)]
    pub nodes: Vec<String>,
    /// Public docs page the semantics were taken from.
    #[serde(default)]
    pub docs: Option<String>,
    /// Name of the node that emits `trigger.json`. Default: first node of the workflow.
    #[serde(default)]
    pub trigger_node: Option<String>,
}

/// n8n runData: node name -> one entry per run.
pub type RunData = IndexMap<String, Vec<NodeRun>>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeRun {
    pub data: RunDataMain,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RunDataMain {
    /// One slot per output; `null` = output never produced (distinct from `[]`).
    #[serde(default)]
    pub main: Vec<Option<Vec<Item>>>,
}

#[derive(Debug, Clone)]
pub struct Case {
    pub name: String,
    pub dir: PathBuf,
    pub meta: CaseMeta,
    pub workflow: Workflow,
    pub trigger: Vec<Item>,
    pub expected: RunData,
}

fn read_json(path: &Path) -> Result<Value, HarnessError> {
    let s = fs::read_to_string(path).map_err(|e| case_err(path, e))?;
    serde_json::from_str(&s).map_err(|e| case_err(path, e))
}

impl Case {
    pub fn load(dir: &Path) -> Result<Self, HarnessError> {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let wf_path = dir.join("workflow.json");
        let wf_src = fs::read_to_string(&wf_path).map_err(|e| case_err(&wf_path, e))?;
        let workflow = Workflow::from_json_str(&wf_src).map_err(|e| case_err(&wf_path, e))?;

        let tr_path = dir.join("trigger.json");
        let trigger = match read_json(&tr_path)? {
            Value::Array(a) => a.iter().map(Item::from_pinned).collect(),
            _ => return Err(case_err(&tr_path, "trigger payload must be a json array")),
        };

        let rd_path = dir.join("runData.json");
        let expected: RunData =
            serde_json::from_value(read_json(&rd_path)?).map_err(|e| case_err(&rd_path, e))?;

        let meta_path = dir.join("meta.json");
        let meta = if meta_path.exists() {
            serde_json::from_value(read_json(&meta_path)?).map_err(|e| case_err(&meta_path, e))?
        } else {
            CaseMeta::default()
        };

        Ok(Self {
            name,
            dir: dir.to_path_buf(),
            meta,
            workflow,
            trigger,
            expected,
        })
    }

    pub fn trigger_node(&self) -> Option<&str> {
        self.meta
            .trigger_node
            .as_deref()
            .or_else(|| self.workflow.nodes.first().map(|n| n.name.as_str()))
    }

    /// Short node types under test, lowercased (`set`, `merge`, ...).
    pub fn node_types(&self) -> Vec<String> {
        let raw: Vec<&str> = if self.meta.nodes.is_empty() {
            self.workflow
                .nodes
                .iter()
                .map(|n| n.node_type.as_str())
                .collect()
        } else {
            self.meta.nodes.iter().map(String::as_str).collect()
        };
        raw.into_iter().map(short_type).collect()
    }

    /// Fixture sanity independent of any executor. Empty = ok.
    pub fn static_problems(&self) -> Vec<String> {
        let mut p = Vec::new();
        if let Err(e) = self.workflow.validate() {
            p.push(format!("workflow invalid: {e}"));
        }
        match self.trigger_node() {
            None => p.push("workflow has no nodes".into()),
            Some(t) => {
                if self.workflow.node(t).is_none() {
                    p.push(format!("trigger node `{t}` not in workflow"));
                }
                if !self.expected.contains_key(t) {
                    p.push(format!("runData has no entry for trigger node `{t}`"));
                }
            }
        }
        for (node, runs) in &self.expected {
            if self.workflow.node(node).is_none() {
                p.push(format!("runData names unknown node `{node}`"));
            }
            if runs.is_empty() {
                p.push(format!(
                    "runData `{node}` has zero runs (omit node instead)"
                ));
            }
        }
        if self.meta.description.is_empty() {
            p.push("meta.description missing".into());
        }
        p
    }
}

fn short_type(t: &str) -> String {
    t.strip_prefix(N8N_BASE_PREFIX).unwrap_or(t).to_lowercase()
}

/// Load every case dir under `root`, sorted by name.
pub fn load_cases(root: &Path) -> Result<Vec<Case>, HarnessError> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
        .map_err(|e| case_err(root, e))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.iter().map(|d| Case::load(d)).collect()
}

// ------------------------------------------------------------------- filter

/// Narrow the corpus: env `HK_N8N_NODE` (comma list of node types, `set`,
/// `n8n-nodes-base.merge`) and `HK_N8N_CASE` (substring of case name).
#[derive(Debug, Clone, Default)]
pub struct CaseFilter {
    pub nodes: Vec<String>,
    pub case: Option<String>,
}

impl CaseFilter {
    pub fn from_env() -> Self {
        let nodes = std::env::var("HK_N8N_NODE")
            .map(|v| Self::parse_nodes(&v))
            .unwrap_or_default();
        let case = std::env::var("HK_N8N_CASE").ok().filter(|s| !s.is_empty());
        Self { nodes, case }
    }

    pub fn parse_nodes(s: &str) -> Vec<String> {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(short_type)
            .collect()
    }

    pub fn matches(&self, case: &Case) -> bool {
        if let Some(c) = &self.case
            && !case.name.contains(c.as_str())
        {
            return false;
        }
        if self.nodes.is_empty() {
            return true;
        }
        let have = case.node_types();
        self.nodes.iter().any(|n| have.contains(n))
    }

    pub fn apply(&self, cases: Vec<Case>) -> Vec<Case> {
        cases.into_iter().filter(|c| self.matches(c)).collect()
    }
}

// ------------------------------------------------------------------- runner

/// What the executor gets: no expectations, only inputs.
#[derive(Debug)]
pub struct RunInput<'a> {
    pub workflow: &'a Workflow,
    pub trigger_node: &'a str,
    /// Items the trigger node emits.
    pub trigger_items: &'a [Item],
}

#[derive(Debug)]
pub enum Execution {
    /// No executor yet.
    Pending,
    Done(RunData),
    /// Executor ran and failed outright.
    Failed(String),
}

#[async_trait]
pub trait FlowRunner: Send + Sync {
    async fn run(&self, input: RunInput<'_>) -> Execution;
}

/// Stand-in until P4.2 lands the engine.
#[derive(Debug, Default, Clone, Copy)]
pub struct PendingRunner;

#[async_trait]
impl FlowRunner for PendingRunner {
    async fn run(&self, _input: RunInput<'_>) -> Execution {
        Execution::Pending
    }
}

// --------------------------------------------------------------------- diff

#[derive(Debug, Clone, PartialEq)]
pub struct NodeDiff {
    pub node: String,
    pub lines: Vec<String>,
}

/// Structural JSON diff; paths are `$.a.b[2]`. Object key order is ignored.
pub fn json_diff(path: &str, expected: &Value, actual: &Value, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => {
            for (k, ev) in e {
                match a.get(k) {
                    Some(av) => json_diff(&format!("{path}.{k}"), ev, av, out),
                    None => out.push(format!("{path}.{k}: missing, expected {ev}")),
                }
            }
            for (k, av) in a {
                if !e.contains_key(k) {
                    out.push(format!("{path}.{k}: unexpected, got {av}"));
                }
            }
        }
        (Value::Array(e), Value::Array(a)) => {
            if e.len() != a.len() {
                out.push(format!(
                    "{path}: array length expected {}, got {}",
                    e.len(),
                    a.len()
                ));
            }
            for (i, (ev, av)) in e.iter().zip(a).enumerate() {
                json_diff(&format!("{path}[{i}]"), ev, av, out);
            }
        }
        (e, a) if e == a => {}
        (e, a) => out.push(format!("{path}: expected {e}, got {a}")),
    }
}

fn diff_node_runs(expected: &[NodeRun], actual: &[NodeRun]) -> Vec<String> {
    let mut out = Vec::new();
    if expected.len() != actual.len() {
        out.push(format!(
            "run count expected {}, got {}",
            expected.len(),
            actual.len()
        ));
    }
    for (r, (e, a)) in expected.iter().zip(actual).enumerate() {
        let (em, am) = (&e.data.main, &a.data.main);
        if em.len() != am.len() {
            out.push(format!(
                "run {r}: output count expected {}, got {}",
                em.len(),
                am.len()
            ));
        }
        for (o, (eo, ao)) in em.iter().zip(am).enumerate() {
            let pre = format!("run {r} out {o}");
            match (eo, ao) {
                (None, None) => {}
                (Some(_), None) => out.push(format!("{pre}: expected items, got null")),
                (None, Some(_)) => out.push(format!("{pre}: expected null, got items")),
                (Some(ei), Some(ai)) => {
                    if ei.len() != ai.len() {
                        out.push(format!(
                            "{pre}: item count expected {}, got {}",
                            ei.len(),
                            ai.len()
                        ));
                    }
                    for (i, (x, y)) in ei.iter().zip(ai).enumerate() {
                        let mut d = Vec::new();
                        json_diff(
                            "$",
                            &Value::Object(x.json.clone()),
                            &Value::Object(y.json.clone()),
                            &mut d,
                        );
                        out.extend(d.into_iter().map(|l| format!("{pre} item {i}: {l}")));
                    }
                }
            }
        }
        if e.error != a.error {
            out.push(format!(
                "run {r}: error expected {:?}, got {:?}",
                e.error, a.error
            ));
        }
    }
    out
}

/// Per-node diff of expected vs actual runData. Empty = equal.
pub fn diff_run_data(expected: &RunData, actual: &RunData) -> Vec<NodeDiff> {
    let mut out = Vec::new();
    for (node, e) in expected {
        let lines = match actual.get(node) {
            None => vec!["node did not run".to_string()],
            Some(a) => diff_node_runs(e, a),
        };
        if !lines.is_empty() {
            out.push(NodeDiff {
                node: node.clone(),
                lines,
            });
        }
    }
    for node in actual.keys().filter(|n| !expected.contains_key(*n)) {
        out.push(NodeDiff {
            node: node.clone(),
            lines: vec!["node ran but was not expected to".to_string()],
        });
    }
    out
}

// ------------------------------------------------------------------- report

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Pass,
    /// `pending_executor` case, executor not available.
    Pending,
    Fail(Vec<NodeDiff>),
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaseReport {
    pub name: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub cases: Vec<CaseReport>,
}

impl Report {
    pub fn count(&self, f: impl Fn(&Outcome) -> bool) -> usize {
        self.cases.iter().filter(|c| f(&c.outcome)).count()
    }

    pub fn failed(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Fail(_) | Outcome::Error(_)))
    }

    pub fn pending(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Pending))
    }

    pub fn passed(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Pass))
    }

    pub fn is_ok(&self) -> bool {
        self.failed() == 0
    }

    /// Human report: one line per case, per-node diff lines under failures.
    pub fn render(&self) -> String {
        let mut s = String::new();
        for c in &self.cases {
            match &c.outcome {
                Outcome::Pass => s.push_str(&format!("PASS    {}\n", c.name)),
                Outcome::Pending => s.push_str(&format!("PENDING {}\n", c.name)),
                Outcome::Error(e) => s.push_str(&format!("ERROR   {}: {e}\n", c.name)),
                Outcome::Fail(diffs) => {
                    s.push_str(&format!("FAIL    {}\n", c.name));
                    for d in diffs {
                        s.push_str(&format!("  node `{}`\n", d.node));
                        for l in &d.lines {
                            s.push_str(&format!("    {l}\n"));
                        }
                    }
                }
            }
        }
        s.push_str(&format!(
            "n8n corpus: {} pass, {} pending, {} fail\n",
            self.passed(),
            self.pending(),
            self.failed()
        ));
        s
    }
}

pub async fn run_case(runner: &dyn FlowRunner, case: &Case) -> CaseReport {
    let name = case.name.clone();
    let Some(trigger_node) = case.trigger_node() else {
        return CaseReport {
            name,
            outcome: Outcome::Error("workflow has no nodes".into()),
        };
    };
    let exec = runner
        .run(RunInput {
            workflow: &case.workflow,
            trigger_node,
            trigger_items: &case.trigger,
        })
        .await;
    let outcome = match exec {
        Execution::Pending if case.meta.status == CaseStatus::PendingExecutor => Outcome::Pending,
        Execution::Pending => {
            Outcome::Error("executor pending but case is `active`; mark it pending_executor".into())
        }
        Execution::Failed(m) => Outcome::Error(format!("executor failed: {m}")),
        Execution::Done(actual) => {
            let d = diff_run_data(&case.expected, &actual);
            if d.is_empty() {
                Outcome::Pass
            } else {
                Outcome::Fail(d)
            }
        }
    };
    CaseReport { name, outcome }
}

pub async fn run_corpus(runner: &dyn FlowRunner, cases: &[Case]) -> Report {
    let mut r = Report::default();
    for c in cases {
        r.cases.push(run_case(runner, c).await);
    }
    r
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn rd(v: Value) -> RunData {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn json_diff_paths() {
        let mut d = Vec::new();
        json_diff(
            "$",
            &json!({"a": {"b": [1, 2]}, "c": 1}),
            &json!({"a": {"b": [1, 3]}, "z": 0}),
            &mut d,
        );
        assert_eq!(
            d,
            vec![
                "$.a.b[1]: expected 2, got 3",
                "$.c: missing, expected 1",
                "$.z: unexpected, got 0"
            ]
        );
    }

    #[test]
    fn run_data_diff_per_node() {
        let exp = rd(json!({
            "A": [{"data": {"main": [[{"json": {"x": 1}}]]}}],
            "B": [{"data": {"main": [[{"json": {"x": 1}}], []]}}],
        }));
        let act = rd(json!({
            "A": [{"data": {"main": [[{"json": {"x": 2}}]]}}],
            "B": [{"data": {"main": [[{"json": {"x": 1}}], []]}}],
            "C": [{"data": {"main": [[]]}}],
        }));
        let d = diff_run_data(&exp, &act);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].node, "A");
        assert_eq!(
            d[0].lines,
            vec!["run 0 out 0 item 0: $.x: expected 1, got 2"]
        );
        assert_eq!(d[1].node, "C");
        assert!(diff_run_data(&exp, &exp).is_empty());
    }

    #[test]
    fn node_filter_normalizes() {
        assert_eq!(
            CaseFilter::parse_nodes("n8n-nodes-base.Merge, set"),
            vec!["merge", "set"]
        );
    }
}
