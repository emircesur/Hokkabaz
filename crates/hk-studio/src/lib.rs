//! hk-studio: long video → short clips.
//!
//! * [`project`] / [`edl`] / [`edit`]: the edit decision list, word-level text
//!   editing, undo/redo.
//! * [`live`]: a shared editing session that broadcasts every change.
//! * [`captions`], [`reframe`], [`broll`], [`templates`], [`social`]: the look.
//! * [`highlights`]: LLM highlight detection and virality scoring.
//! * [`pipeline`]: import, transcribe, analyze, preview, render (ffmpeg via `hk-media`).

pub mod brand;
pub mod broll;
pub mod captions;
pub mod concat_bridge;
pub mod edit;
pub mod edl;
mod error;
pub mod highlights;
pub mod host;
pub mod live;
pub mod llm;
pub mod longform;
pub mod pipeline;
pub mod project;
pub mod publish;
pub mod reframe;
pub mod service;
pub mod social;
pub mod templates;
pub mod tools;
pub mod vision;
pub mod whisper_models;

pub use error::{Result, StudioError};
pub use host::{AppBackends, SharedSettings, StudioHost, StudioSettings};
pub use live::{LiveStudio, StudioEvent};
pub use pipeline::{PreviewPlan, PreviewWord, RenderOpts, Studio};
pub use project::{Clip, ProjectStore, StudioProject};
pub use service::{Backends, ProjectSummary, StudioService};
pub use tools::studio_tools;
