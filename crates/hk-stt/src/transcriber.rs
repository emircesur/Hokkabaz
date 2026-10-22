//! The [`Transcriber`] trait.

use std::path::Path;

use async_trait::async_trait;

use crate::error::Result;
use crate::transcript::Transcript;

/// Per-call options.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscribeOpts {
    /// Language hint (ISO code); `None` = auto-detect or config default.
    pub language: Option<String>,
    /// Vocabulary / style prompt passed to the model.
    pub prompt: Option<String>,
    /// Ask for word timestamps (always interpolated when the server only gives segments).
    pub word_timestamps: bool,
}

impl Default for TranscribeOpts {
    fn default() -> Self {
        Self { language: None, prompt: None, word_timestamps: true }
    }
}

/// Speech-to-text engine. Object safe.
#[async_trait]
pub trait Transcriber: Send + Sync {
    /// Transcribe one audio file. Times are relative to the file start.
    async fn transcribe(&self, audio_path: &Path, opts: &TranscribeOpts) -> Result<Transcript>;

    /// Largest file this engine accepts in one call, if limited. Lets
    /// [`crate::ChunkedTranscriber`] decide when to split.
    fn max_upload_bytes(&self) -> Option<u64> {
        None
    }
}
