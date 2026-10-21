//! Node trait + registry keyed by `(type, typeVersion)`.
//!
//! [`NodeContext`] is what the executor (`executor.rs`) hands a node per run; the
//! expression scope (P4.3) and sub-node inputs will grow it further.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde_json::Value;

use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::hooks::{ExecScratch, NodeHooks};
use crate::expr::{ExprEnv, ExprError, ExprScope, ExpressionEvaluator, ScopeBase, resolve_value};
use crate::item::Item;
use crate::run_data::{RunSource, SharedRunData, StaticData};
use crate::version::{NodeKey, Version};
use crate::workflow::Node;

/// Items per output port. An empty inner vec means "nothing on that branch".
pub type NodeOutput = Vec<Vec<Item>>;

/// What a node implementation sees for one run.
#[derive(Debug, Clone)]
pub struct NodeContext {
    pub node: Node,
    /// Items per input port.
    pub input: Vec<Vec<Item>>,
    pub run_index: usize,
    pub workflow_id: Option<String>,
    pub workflow_name: Option<String>,
    pub execution_id: Option<String>,
    /// Where each input port's items came from.
    pub source: Vec<Option<RunSource>>,
    /// Results of nodes that already ran in this execution.
    pub run_data: SharedRunData,
    /// `getWorkflowStaticData`; scopes `global` and `node:<name>` (see [`node_scope`](crate::run_data::node_scope)).
    pub static_data: StaticData,
    /// Expression engine inputs (`$env`, `$vars`, evaluator override, ...). Set before the
    /// first `eval_param`: the scope snapshot is taken once per context.
    pub expr: ExprEnv,
    /// Embedder hooks (sub-workflows, webhook response, credentials).
    pub hooks: NodeHooks,
    /// Per-execution scratch state (Loop Over Items queue).
    pub scratch: ExecScratch,
    expr_base: OnceLock<Arc<ScopeBase>>,
}

impl NodeContext {
    /// Context outside an execution (tests, one-off node runs): empty run data/static data.
    pub fn new(node: Node, input: Vec<Vec<Item>>, run_index: usize) -> Self {
        Self {
            node,
            input,
            run_index,
            workflow_id: None,
            workflow_name: None,
            execution_id: None,
            source: Vec::new(),
            run_data: SharedRunData::new(),
            static_data: StaticData::default(),
            expr: ExprEnv::default(),
            hooks: NodeHooks::default(),
            scratch: ExecScratch::new(),
            expr_base: OnceLock::new(),
        }
    }

    /// Copy with one extra `$vars` entry and a fresh scope snapshot. HTTP Request
    /// pagination exposes the last response this way (`$response` is rewritten to
    /// `$vars.<name>` by the node).
    pub fn with_extra_var(&self, name: &str, value: Value) -> Self {
        let mut c = self.clone();
        let mut vars = (*c.expr.vars).clone();
        vars.insert(name.to_owned(), value);
        c.expr.vars = Arc::new(vars);
        c.expr_base = OnceLock::new();
        c
    }

    /// Expression scope for `item_index`. The heavy part (input items, run data handle)
    /// is built once per context and shared, so evaluating per item stays O(1) to set up.
    pub fn expr_scope(&self, item_index: usize) -> ExprScope {
        let base = self.expr_base.get_or_init(|| {
            let e = &self.expr;
            Arc::new(ScopeBase {
                node_name: self.node.name.clone(),
                node_params: Value::Object(self.node.parameters.clone()),
                input: self.input.clone(),
                source: self.source.clone(),
                run_index: self.run_index,
                run_data: self.run_data.clone(),
                workflow_id: self.workflow_id.clone(),
                workflow_name: self.workflow_name.clone(),
                workflow_active: e.workflow_active,
                execution_id: self.execution_id.clone(),
                mode: e.mode.clone().unwrap_or_else(|| "manual".into()),
                resume_url: e.resume_url.clone(),
                env: Arc::clone(&e.env),
                vars: Arc::clone(&e.vars),
                node_params_all: Arc::clone(&e.node_params),
                timezone: e.timezone.clone().unwrap_or_else(|| "UTC".into()),
                now_ms: e.now_ms,
            })
        });
        ExprScope::new(Arc::clone(base), item_index)
    }

    fn evaluator(&self) -> Result<Arc<dyn ExpressionEvaluator>, ExprError> {
        if let Some(e) = &self.expr.evaluator {
            return Ok(Arc::clone(e));
        }
        #[cfg(feature = "expr")]
        {
            Ok(crate::expr::default_evaluator())
        }
        #[cfg(not(feature = "expr"))]
        {
            Err(ExprError::Unavailable(
                "hk-flow built without the `expr` feature".into(),
            ))
        }
    }

    /// Resolve a raw parameter value for one input item: strings starting with `=` are
    /// n8n expressions (`={{ $json.a }}`, `=Hi {{ $json.name }}`), objects/arrays are
    /// resolved recursively, everything else passes through.
    pub fn eval_param(&self, value: &Value, item_index: usize) -> Result<Value, FlowError> {
        let scope = self.expr_scope(item_index);
        let mut engine: Option<Arc<dyn ExpressionEvaluator>> = None;
        resolve_value(value, &mut |tpl| {
            if engine.is_none() {
                engine = Some(self.evaluator()?);
            }
            match &engine {
                Some(e) => e.evaluate(tpl, &scope),
                None => Err(ExprError::Unavailable("no evaluator".into())),
            }
        })
        .map_err(|e| FlowError::Node {
            node: self.node.name.clone(),
            message: e.to_string(),
        })
    }

    /// Evaluated parameter at a dotted `path` in `node.parameters` (`options.timeout`).
    /// `None` when the parameter is absent.
    pub fn eval_param_at(&self, path: &str, item_index: usize) -> Result<Option<Value>, FlowError> {
        let mut parts = path.split('.');
        let Some(first) = parts.next() else {
            return Ok(None);
        };
        let mut cur = match self.node.parameters.get(first) {
            Some(v) => v,
            None => return Ok(None),
        };
        for p in parts {
            cur = match cur {
                Value::Object(m) => match m.get(p) {
                    Some(v) => v,
                    None => return Ok(None),
                },
                Value::Array(a) => match p.parse::<usize>().ok().and_then(|i| a.get(i)) {
                    Some(v) => v,
                    None => return Ok(None),
                },
                _ => return Ok(None),
            };
        }
        self.eval_param(cur, item_index).map(Some)
    }
}

#[async_trait]
pub trait FlowNode: Send + Sync + fmt::Debug {
    fn description(&self) -> &NodeTypeDescription;

    /// Versions this implementation serves. One impl may cover several `typeVersion`s
    /// (n8n versioned nodes branch on `typeVersion` internally).
    fn versions(&self) -> Vec<Version> {
        self.description().version.to_vec()
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError>;
}

/// Lookup of implementations. Unknown keys are not an import error: the workflow
/// keeps the node as a placeholder and only running it fails ([`FlowError::UnsupportedNode`]).
#[derive(Debug, Default)]
pub struct NodeRegistry {
    by_type: HashMap<String, BTreeMap<Version, Arc<dyn FlowNode>>>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register under every version the node declares.
    pub fn register(&mut self, node: Arc<dyn FlowNode>) -> Result<(), FlowError> {
        let ty = node.description().name.clone();
        let versions = node.versions();
        let slot = self.by_type.entry(ty.clone()).or_default();
        if let Some(v) = versions.iter().find(|v| slot.contains_key(v)) {
            return Err(FlowError::Invalid(format!(
                "node {ty}@{v} registered twice"
            )));
        }
        for v in versions {
            slot.insert(v, Arc::clone(&node));
        }
        Ok(())
    }

    /// Exact `(type, version)` match; n8n does not round versions.
    pub fn get(&self, key: &NodeKey) -> Option<&Arc<dyn FlowNode>> {
        self.by_type.get(&key.node_type)?.get(&key.version)
    }

    pub fn resolve(&self, node: &Node) -> Result<&Arc<dyn FlowNode>, FlowError> {
        let key = node.key();
        self.get(&key).ok_or(FlowError::UnsupportedNode(key))
    }

    pub fn latest(&self, node_type: &str) -> Option<&Arc<dyn FlowNode>> {
        self.by_type.get(node_type)?.values().next_back()
    }

    pub fn is_supported(&self, node: &Node) -> bool {
        self.get(&node.key()).is_some()
    }

    pub fn types(&self) -> impl Iterator<Item = &str> {
        self.by_type.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[derive(Debug)]
    struct Noop(NodeTypeDescription);

    #[async_trait]
    impl FlowNode for Noop {
        fn description(&self) -> &NodeTypeDescription {
            &self.0
        }
        async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
            Ok(ctx.input.clone())
        }
    }

    fn noop(versions: serde_json::Value) -> Arc<dyn FlowNode> {
        let d = serde_json::from_value(json!({
            "displayName": "No Op", "name": "n8n-nodes-base.noOp", "version": versions
        }))
        .unwrap();
        Arc::new(Noop(d))
    }

    fn node(ver: f64) -> Node {
        Node::new(
            "x",
            "n8n-nodes-base.noOp",
            serde_json::Number::from_f64(ver).unwrap(),
            [0.into(), 0.into()],
        )
    }

    #[tokio::test]
    async fn keyed_by_type_and_version() {
        let mut r = NodeRegistry::new();
        r.register(noop(json!([1, 1.1]))).unwrap();
        assert!(r.is_supported(&node(1.1)));
        assert!(!r.is_supported(&node(2.0)));
        assert!(matches!(
            r.resolve(&node(2.0)),
            Err(FlowError::UnsupportedNode(_))
        ));
        assert!(r.register(noop(json!(1))).is_err());

        let n = r.resolve(&node(1.0)).unwrap();
        let ctx = NodeContext::new(node(1.0), vec![vec![Item::from_json(json!({"a": 1}))]], 0);
        assert_eq!(n.execute(&ctx).await.unwrap().len(), 1);
        assert_eq!(
            r.latest("n8n-nodes-base.noOp").unwrap().description().name,
            "n8n-nodes-base.noOp"
        );
    }
}
