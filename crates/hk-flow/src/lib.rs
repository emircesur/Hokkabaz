//! hk-flow: n8n-compatible workflow model.
//!
//! The native workflow format *is* n8n workflow JSON (DESIGN §3.9). Everything here
//! round-trips losslessly: fields we do not model are kept in `extra` maps and
//! re-emitted on export, so a workflow authored in n8n exports back unchanged.
//!
//! Clean-room: shapes derived from the public n8n docs and exported JSON only.

// n8n node descriptions are large `json!` literals
#![recursion_limit = "512"]

pub mod description;
pub mod error;
pub mod executor;
pub mod expr;
pub mod hooks;
pub mod item;
pub mod node;
pub mod nodes;
pub mod raw;
pub mod run_data;
#[cfg(feature = "testing")]
pub mod testing;
pub mod triggers;
pub mod version;
pub mod workflow;

pub use description::{
    CredentialDescription, DisplayOptions, IoDecl, IoSpec, NodeDefaults, NodeProperty,
    NodeTypeDescription, PropertyType,
};
pub use error::FlowError;
pub use executor::{
    ExecuteInput, ExecutionOrder, ExecutionResult, Executor, ExecutorConfig, ResumeReason,
    RunControl, WaitSnapshot,
};
#[cfg(feature = "expr")]
pub use expr::QuickJsEvaluator;
pub use expr::{
    ExprEnv, ExprError, ExprLimits, ExprScope, ExpressionEvaluator, ScriptMode, ScriptOutput,
};
pub use hooks::{
    CollectingSink, CredentialResolver, ExecScratch, NodeHooks, RemoteNodeRunner, ResponseBody,
    ResponseSink, SubWorkflowRequest, SubWorkflowRunner, WebhookResponse,
};
pub use item::{BinaryData, Item, PairedItem, PairedSource};
pub use node::{FlowNode, NodeContext, NodeOutput, NodeRegistry, SubNodeEnv, Supplied};
pub use nodes::register_core_nodes;
pub use run_data::{
    ExecutionRecord, InMemoryRunStore, NodeRun, NodeRunData, RunData, RunError, RunSource,
    RunStatus, RunStore, SharedRunData, StaticData,
};
pub use triggers::wait::{InMemoryWaitStore, WaitRecord, WaitRequest, WaitStore, WaitTrigger};
pub use triggers::{
    LocalTriggerHost, TriggerEvent, TriggerHost, TriggerSink, TriggerSpec, WebhookRegistry,
    WebhookRequest,
};
pub use version::{NodeKey, NodeVersions, Version};
pub use workflow::{
    ConnectionTarget, Connections, CredentialRef, ErrorMode, MAIN, Node, NodeConnections, PinData,
    Settings, Workflow,
};
