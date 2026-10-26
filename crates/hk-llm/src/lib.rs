//! hk-llm: provider-neutral LLM interface.
//!
//! Adapters (deepseek, openai-compat, ...) implement [`Provider`] and normalise
//! their wire format into [`StreamEvent`]s. The agent loop and router only see
//! the types in this crate.
// port: dsh packages/core/llm (stream vocabulary), opencode packages/opencode/src/provider

pub mod anthropic;
pub mod catalog;
pub mod deepseek;
mod error;
mod event;
pub mod gemini;
mod model;
pub mod ollama;
pub mod openai;
mod params;
mod provider;
pub mod registry;
mod request;
pub mod router;
pub mod structured;
mod usage;

pub use error::{LlmError, Result};
pub use event::{FinishReason, StreamEvent};
pub use model::{Capabilities, ModelCost, ModelInfo};
pub use params::supports_param;
pub use provider::{EventStream, Provider};
pub use registry::{
    KeyRef, KeyResolver, ModelList, ProviderConfig, ProviderKind, ProviderOptions,
    ProviderRegistry, RegistryError, RouteConfig, build_router,
};
pub use request::{
    ChatRequest, Message, ReasoningEffort, Role, SamplingParams, ToolCall, ToolChoice, ToolSpec,
};
pub use router::{AutoRoutes, Budget, Meter, Projection, Route, RouteTotals, Router, TaskKind};
pub use tokio_util::sync::CancellationToken;
pub use usage::{Cost, Usage};
