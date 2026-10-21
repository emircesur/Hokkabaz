//! Node trait + registry keyed by `(type, typeVersion)`.
//!
//! P4.1 fixes the shape only; the executor (P4.2) fleshes out [`NodeContext`]
//! (expression scope, static data, sub-node inputs, hooks).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::Item;
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
        let ctx = NodeContext {
            node: node(1.0),
            input: vec![vec![Item::from_json(json!({"a": 1}))]],
            run_index: 0,
            workflow_id: None,
            workflow_name: None,
        };
        assert_eq!(n.execute(&ctx).await.unwrap().len(), 1);
        assert_eq!(
            r.latest("n8n-nodes-base.noOp").unwrap().description().name,
            "n8n-nodes-base.noOp"
        );
    }
}
