//! hk-flow: n8n-compatible workflow model.
//!
//! The native workflow format *is* n8n workflow JSON (DESIGN §3.9). Everything here
//! round-trips losslessly: fields we do not model are kept in `extra` maps and
//! re-emitted on export, so a workflow authored in n8n exports back unchanged.
//!
//! Clean-room: shapes derived from the public n8n docs and exported JSON only.

pub mod description;
pub mod error;
pub mod item;
pub mod node;
pub mod raw;
#[cfg(feature = "testing")]
pub mod testing;
pub mod version;
pub mod workflow;

pub use description::{
    CredentialDescription, DisplayOptions, IoDecl, IoSpec, NodeDefaults, NodeProperty,
    NodeTypeDescription, PropertyType,
};
pub use error::FlowError;
pub use item::{BinaryData, Item, PairedItem, PairedSource};
pub use node::{FlowNode, NodeContext, NodeOutput, NodeRegistry};
pub use version::{NodeKey, NodeVersions, Version};
pub use workflow::{
    ConnectionTarget, Connections, CredentialRef, ErrorMode, MAIN, Node, NodeConnections, PinData,
    Settings, Workflow,
};
