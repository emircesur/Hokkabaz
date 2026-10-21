//! The [`Provider`] trait implemented by every adapter.

use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use crate::{ChatRequest, ModelInfo, Result, StreamEvent};

/// Stream of normalised events for one request.
pub type EventStream = BoxStream<'static, Result<StreamEvent>>;

/// An LLM backend. Object-safe so the router can hold `Arc<dyn Provider>`.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Stable provider id, e.g. `deepseek`.
    fn id(&self) -> &str;

    /// Models this provider currently offers.
    async fn models(&self) -> Result<Vec<ModelInfo>>;

    /// Start a streaming completion.
    ///
    /// Cancelling `cancel` must abort the HTTP request promptly; the stream
    /// then ends with [`crate::LlmError::Cancelled`]. A successful stream
    /// ends with exactly one [`StreamEvent::Finish`].
    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream>;
}
