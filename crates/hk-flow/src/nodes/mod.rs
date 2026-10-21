//! Core n8n nodes, keyed by `(type, typeVersion)`.
//!
//! Clean-room: behavior from the public n8n docs and exported workflow JSON. Each node
//! carries an n8n-shaped [`NodeTypeDescription`] so the canvas can render its parameter
//! form. Parameters are read with `ctx.eval_param*`, so expressions work everywhere.
//!
//! | node | type | versions |
//! |---|---|---|
//! | Edit Fields (Set) | `set` | 3 - 3.4 (manual + JSON) |
//! | If / Filter | `if` / `filter` | 2 - 2.2 |
//! | Switch | `switch` | 3 - 3.2 |
//! | Merge | `merge` | 3 - 3.2 (no SQL mode) |
//! | Loop Over Items | `splitInBatches` | 3 |
//! | No Operation | `noOp` | 1 |
//! | Date & Time | `dateTime` | 2 |
//! | Aggregate, Split Out, Sort, Limit | | 1 |
//! | Remove Duplicates | `removeDuplicates` | 1, 1.1, 2 (current-input only) |
//! | Wait | `wait` | 1, 1.1 (inline sleep; long, webhook, form = durable, see `triggers::wait`) |
//! | Execute Workflow | `executeWorkflow` | 1 - 1.2 (needs a [`SubWorkflowRunner`](crate::hooks::SubWorkflowRunner)) |
//! | Respond to Webhook | `respondToWebhook` | 1 - 1.3 (emits to a [`ResponseSink`](crate::hooks::ResponseSink)) |
//! | HTTP Request | `httpRequest` | 4 - 4.2 (feature `http`) |
//! | Manual / Schedule / Webhook / Form / Chat / Execute Workflow / Error triggers | | see `triggers/` |
//! | App Event / File Watch triggers | `hokkabaz.*` | 1 (Hokkabaz native) |

use std::sync::Arc;

use crate::error::FlowError;
use crate::node::{FlowNode, NodeRegistry};

mod conditions;
pub(crate) mod date;
mod datetime;
mod flow_control;
#[cfg(feature = "http")]
mod http;
mod merge;
mod routing;
mod set;
mod transform;
mod triggers;
mod util;

pub use datetime::DateTimeNode;
pub use flow_control::{ExecuteWorkflowNode, RespondToWebhookNode, WaitNode};
#[cfg(feature = "http")]
pub use http::HttpRequestNode;
pub use merge::MergeNode;
pub use routing::{FilterNode, IfNode, NoOpNode, SwitchNode};
pub use set::SetNode;
pub use transform::{
    AggregateNode, LimitNode, RemoveDuplicatesNode, SortNode, SplitInBatchesNode, SplitOutNode,
};
pub use triggers::TriggerNode;

/// Every core node implementation in this crate.
pub fn core_nodes() -> Vec<Arc<dyn FlowNode>> {
    #[cfg_attr(not(feature = "http"), allow(unused_mut))]
    let mut v: Vec<Arc<dyn FlowNode>> = vec![
        Arc::new(SetNode::new()),
        Arc::new(IfNode::new()),
        Arc::new(SwitchNode::new()),
        Arc::new(FilterNode::new()),
        Arc::new(MergeNode::new()),
        Arc::new(SplitInBatchesNode::new()),
        Arc::new(NoOpNode::new()),
        Arc::new(DateTimeNode::new()),
        Arc::new(AggregateNode::new()),
        Arc::new(SplitOutNode::new()),
        Arc::new(SortNode::new()),
        Arc::new(LimitNode::new()),
        Arc::new(RemoveDuplicatesNode::new()),
        Arc::new(WaitNode::new()),
        Arc::new(ExecuteWorkflowNode::new()),
        Arc::new(RespondToWebhookNode::new()),
    ];
    #[cfg(feature = "http")]
    v.push(Arc::new(HttpRequestNode::new()));
    v.extend(
        triggers::trigger_nodes()
            .into_iter()
            .map(|n| Arc::new(n) as Arc<dyn FlowNode>),
    );
    v
}

/// Register all core nodes. Errors only if a `(type, version)` is already taken.
pub fn register_core_nodes(registry: &mut NodeRegistry) -> Result<(), FlowError> {
    for n in core_nodes() {
        registry.register(n)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_core_nodes_register_once() {
        let mut r = NodeRegistry::new();
        register_core_nodes(&mut r).unwrap();
        assert!(register_core_nodes(&mut r).is_err());
        for t in [
            "set",
            "if",
            "switch",
            "filter",
            "merge",
            "splitInBatches",
            "noOp",
            "dateTime",
            "aggregate",
            "splitOut",
            "sort",
            "limit",
            "removeDuplicates",
            "wait",
            "executeWorkflow",
            "respondToWebhook",
            "manualTrigger",
            "scheduleTrigger",
            "webhook",
            "formTrigger",
            "executeWorkflowTrigger",
            "errorTrigger",
        ] {
            assert!(r.latest(&format!("n8n-nodes-base.{t}")).is_some(), "{t}");
        }
    }
}
