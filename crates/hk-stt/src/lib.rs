//! hk-stt: speech-to-text for Studio.
//!
//! Word-level [`Transcript`] model (with text-editing helpers), the
//! [`Transcriber`] trait, an OpenAI-compatible HTTP engine, a chunking wrapper
//! for long audio, and an optional local whisper.cpp engine (feature `whisper`).

mod chunked;
mod config;
mod error;
mod filler;
mod openai_compat;
mod transcriber;
mod transcript;
#[cfg(feature = "whisper")]
pub mod whisper;

pub use chunked::{AudioSplitter, ChunkOpts, ChunkResult, ChunkedTranscriber, plan_chunks, stitch};
pub use config::{SttConfig, SttProvider, build_chunked_transcriber, build_transcriber};
pub use error::{Result, SttError};
pub use filler::{FillerSet, language_code};
pub use openai_compat::{DEFAULT_MAX_UPLOAD_BYTES, OpenAiCompatOptions, OpenAiCompatTranscriber, parse_verbose_json};
pub use transcriber::{TranscribeOpts, Transcriber};
pub use transcript::{MIN_WORD_DURATION, SearchHit, Segment, SegmentOpts, Transcript, Word, WordFlags};
