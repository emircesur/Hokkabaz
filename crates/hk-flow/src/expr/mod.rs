//! n8n expression engine (DESIGN §3.9): `={{ ... }}` and mixed `text {{ }} text` templates.
//!
//! Split so crates that only need the types (nodes calling `ctx.eval_param`) compile
//! without a JS engine: the trait, scope and template parser are always built, the
//! QuickJS implementation ([`QuickJsEvaluator`]) sits behind the `expr` feature.
//!
//! Clean-room: variable names, extension methods and Luxon subset follow the public n8n
//! docs only. Expressions run in a QuickJS realm with no fs/net/process; only a single
//! host dispatcher (reachable from the shim closure alone) reads the scope.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::item::Item;
use crate::run_data::{RunSource, SharedRunData};

#[cfg(feature = "expr")]
mod engine;
mod template;

#[cfg(feature = "expr")]
pub use engine::{QuickJsEvaluator, default_evaluator};
pub use template::{Part, parse_template};

#[derive(Debug, thiserror::Error)]
pub enum ExprError {
    /// JS exception raised by the expression (syntax error, ReferenceError, shim error).
    #[error("{0}")]
    Js(String),
    #[error("expression timed out")]
    Timeout,
    #[error("expression exceeded the memory limit")]
    Memory,
    #[error("expression engine unavailable: {0}")]
    Unavailable(String),
}

/// Sandbox limits. Memory and stack are per execution session; the timeout is per
/// evaluated expression (interrupt handler aborts runaway loops).
#[derive(Debug, Clone)]
pub struct ExprLimits {
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    pub timeout: Duration,
    /// Execution sessions (JS realm + compiled-expression cache) kept alive at once.
    pub max_sessions: usize,
}

impl Default for ExprLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            stack_bytes: 1024 * 1024,
            timeout: Duration::from_secs(5),
            max_sessions: 16,
        }
    }
}

/// Evaluates the body of an expression parameter (the part after the leading `=`).
/// Blocking: nodes call it from `execute`; work is bounded by [`ExprLimits::timeout`].
pub trait ExpressionEvaluator: Send + Sync + fmt::Debug {
    /// `template` is e.g. `{{ $json.a }}` (typed result) or `Hi {{ $json.a }}` (string).
    fn evaluate(&self, template: &str, scope: &ExprScope) -> Result<Value, ExprError>;

    /// Drop engine state kept for an execution (compiled cache, JS realm).
    fn release(&self, _execution_id: &str) {}
}

/// Evaluator + non-run data a node's expressions may read. Carried on `NodeContext`.
/// Env is empty by default: host environment variables can hold secrets, so the
/// embedder opts in with an allowlist.
#[derive(Debug, Clone, Default)]
pub struct ExprEnv {
    /// `None` falls back to the process-wide default QuickJS evaluator.
    pub evaluator: Option<Arc<dyn ExpressionEvaluator>>,
    /// `$env`
    pub env: Arc<BTreeMap<String, String>>,
    /// `$vars`
    pub vars: Arc<Map<String, Value>>,
    /// Raw parameters of every workflow node (`$('X').params`, `$node.X.parameter`).
    pub node_params: Arc<BTreeMap<String, Value>>,
    /// `$execution.mode` (`manual`, `trigger`, `webhook`, ...).
    pub mode: Option<String>,
    pub workflow_active: bool,
    pub resume_url: Option<String>,
    /// IANA zone or fixed `UTC+2` used by `$now`/`DateTime` (`UTC` when unset).
    pub timezone: Option<String>,
    /// Freeze `$now` (tests); unix ms.
    pub now_ms: Option<i64>,
}

/// Everything an expression can see, shared across the items of one node run.
#[derive(Debug, Clone, Default)]
pub struct ScopeBase {
    pub node_name: String,
    pub node_params: Value,
    pub input: Vec<Vec<Item>>,
    pub source: Vec<Option<RunSource>>,
    pub run_index: usize,
    pub run_data: SharedRunData,
    pub workflow_id: Option<String>,
    pub workflow_name: Option<String>,
    pub workflow_active: bool,
    pub execution_id: Option<String>,
    pub mode: String,
    pub resume_url: Option<String>,
    pub env: Arc<BTreeMap<String, String>>,
    pub vars: Arc<Map<String, Value>>,
    pub node_params_all: Arc<BTreeMap<String, Value>>,
    pub timezone: String,
    pub now_ms: Option<i64>,
}

/// [`ScopeBase`] + the item the expression is evaluated for (`$itemIndex`).
#[derive(Debug, Clone)]
pub struct ExprScope {
    pub base: Arc<ScopeBase>,
    pub item_index: usize,
}

impl ExprScope {
    pub fn new(base: Arc<ScopeBase>, item_index: usize) -> Self {
        Self { base, item_index }
    }

    pub fn at(&self, item_index: usize) -> Self {
        Self {
            base: Arc::clone(&self.base),
            item_index,
        }
    }
}

/// Resolve every `=`-prefixed string in a parameter value (recursing into objects and
/// arrays) with `eval`. Non-expression values pass through untouched; `eval` receives the
/// template with the `=` stripped.
pub fn resolve_value(
    v: &Value,
    eval: &mut dyn FnMut(&str) -> Result<Value, ExprError>,
) -> Result<Value, ExprError> {
    match v {
        Value::String(s) => match s.strip_prefix('=') {
            Some(body) => eval(body),
            None => Ok(v.clone()),
        },
        Value::Array(a) => a
            .iter()
            .map(|x| resolve_value(x, eval))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(m) => {
            let mut out = Map::with_capacity(m.len());
            for (k, x) in m {
                out.insert(k.clone(), resolve_value(x, eval)?);
            }
            Ok(Value::Object(out))
        }
        _ => Ok(v.clone()),
    }
}

/// Run data pre-filled for tests of expression-reading nodes (the executor owns the real
/// write path).
#[cfg(feature = "testing")]
pub fn shared_run_data(rd: crate::run_data::RunData) -> SharedRunData {
    let shared = SharedRunData::new();
    for (name, runs) in rd {
        for run in runs {
            shared.push(&name, run);
        }
    }
    shared
}
