//! Model metadata: capabilities, limits, pricing.

use serde::{Deserialize, Serialize};

use crate::usage::{Cost, Usage};

/// What a model can do; drives UI toggles and router selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Supports tool/function calling.
    pub tools: bool,
    /// Emits reasoning/thinking output.
    pub reasoning: bool,
    /// Accepts image input.
    pub vision: bool,
}

/// Price per million tokens in USD.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCost {
    /// Fresh input tokens.
    pub input: f64,
    /// Output tokens (reasoning included).
    pub output: f64,
    /// Cache-hit input tokens, if the provider discounts them.
    pub cache_read: Option<f64>,
    /// Cache-write input tokens, if the provider charges extra.
    pub cache_write: Option<f64>,
}

impl ModelCost {
    /// Price `usage`. Missing cache prices fall back to the plain input price.
    pub fn price(&self, usage: &Usage) -> Cost {
        const M: f64 = 1_000_000.0;
        let read = self.cache_read.unwrap_or(self.input);
        let write = self.cache_write.unwrap_or(self.input);
        // u64 -> f64 may lose precision above 2^53 tokens; irrelevant for pricing.
        #[allow(clippy::cast_precision_loss)]
        let usd = (usage.input_tokens as f64 * self.input
            + usage.cache_read_tokens as f64 * read
            + usage.cache_write_tokens as f64 * write
            + usage.output_tokens as f64 * self.output)
            / M;
        Cost { usd }
    }
}

/// One model offered by a provider (from the adapter or the models.dev catalog).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Provider-local model id, e.g. `deepseek-chat`.
    pub id: String,
    /// Owning provider id, e.g. `deepseek`.
    pub provider: String,
    /// Human-readable name for UIs.
    pub name: String,
    /// Context window in tokens.
    pub context_window: u64,
    /// Max output tokens per request, when the provider caps it.
    pub max_output: Option<u64>,
    /// Feature flags.
    pub capabilities: Capabilities,
    /// Pricing; `None` for local or unpriced models.
    pub cost: Option<ModelCost>,
}
