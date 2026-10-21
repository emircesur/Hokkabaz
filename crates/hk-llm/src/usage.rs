//! Token usage and cost accounting.

use serde::{Deserialize, Serialize};

/// Token counts reported by a provider for one request.
///
/// Cache fields are separate so cost can price hits and misses differently
/// (DeepSeek prefix cache, Anthropic prompt caching).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt tokens not served from cache.
    pub input_tokens: u64,
    /// Generated tokens, including reasoning tokens.
    pub output_tokens: u64,
    /// Prompt tokens served from cache (cheaper).
    pub cache_read_tokens: u64,
    /// Prompt tokens written to cache (Anthropic-style, priced higher).
    pub cache_write_tokens: u64,
    /// Subset of `output_tokens` spent on reasoning, when reported.
    pub reasoning_tokens: u64,
}

impl Usage {
    /// All prompt-side tokens: fresh + cache reads + cache writes.
    pub fn total_input(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    /// Prompt plus completion tokens.
    pub fn total(&self) -> u64 {
        self.total_input() + self.output_tokens
    }

    /// Add `other` into `self` (session-level meter).
    pub fn accumulate(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
    }
}

/// Monetary cost in USD, derived from [`Usage`] and a model's price table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    /// Total cost in US dollars.
    pub usd: f64,
}

impl Cost {
    /// Add `other` into `self`.
    pub fn accumulate(&mut self, other: &Cost) {
        self.usd += other.usd;
    }
}
