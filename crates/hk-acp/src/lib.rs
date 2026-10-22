//! hk-acp: ACP (Agent Client Protocol) client. Runs external agents (`dsh --profile acp`,
//! `opencode acp`) as an engine behind our chat/session surface. Desktop only: android builds
//! must not depend on this crate (the child-process spawn is also `cfg(not(android))`).
// port: dsh packages/acp, opencode acp (protocol semantics only; spec https://agentclientprotocol.com)

pub mod client;
pub mod engine;
pub mod map;
pub mod proto;
pub mod rpc;
pub mod spec;
pub mod workspace;

pub use engine::{
    AcpEngine, AcpError, ApprovalPrompt, Approver, Decision, DenyAll, EngineEvent, EngineOptions,
    ToolInfo,
};
pub use map::SessionMapper;
pub use proto::StopReason;
pub use spec::{EngineSpec, SpecError};
