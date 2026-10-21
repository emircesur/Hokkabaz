//! hk-llm: provider-neutral LLM interface.
//!
//! Adapters (deepseek, openai-compat, ...) implement [`Provider`] and normalise
//! their wire format into [`StreamEvent`]s. The agent loop and router only see
//! the types in this crate.
// port: dsh packages/core/llm (stream vocabulary), opencode packages/opencode/src/provider

mod error;
mod event;
mod model;
mod provider;
mod request;
mod usage;

pub use error::{LlmError, Result};
pub use event::{FinishReason, StreamEvent};
pub use model::{Capabilities, ModelCost, ModelInfo};
pub use provider::{EventStream, Provider};
pub use request::{
    ChatRequest, Message, ReasoningEffort, Role, ToolCall, ToolChoice, ToolSpec,
};
pub use tokio_util::sync::CancellationToken;
pub use usage::{Cost, Usage};
