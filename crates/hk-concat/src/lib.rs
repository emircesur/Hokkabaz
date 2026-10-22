//! hk-concat: bridge to the included Concat video editor (DESIGN D16, docs/CONCAT.md).
//!
//! Layers, bottom up:
//! - [`ConcatBackend`]: Concat's MCP tool surface (name + JSON args -> JSON). Implemented by
//!   [`ProcessBackend`] (`concat --mcp` child; desktop) and, behind cargo feature `engine`, by an
//!   in-process `EngineBackend`.
//! - [`ConcatClient`]: typed wrapper (projects, media, cuts, titles/captions, keyframes, effects,
//!   frames, export).
//! - [`plan`]: Studio EDL ([`StudioClipPlan`]) <-> Concat project.
//! - [`agent_tools`]: `concat_*` agent tools for `AgentRuntime::add_tools`.
//!
//! Android: no `concat` binary; [`ProcessBackend::spawn`] returns [`ConcatError::Unsupported`].
// port: none (Hokkabaz-native bridge; Concat is used as a separate AGPL program/library, see
// docs/UPSTREAM.md and third_party/concat/TRADEMARK.md)

pub mod agent_tools;
mod backend;
mod client;
mod config;
#[cfg(feature = "engine")]
pub mod engine;
mod error;
mod locate;
pub mod plan;
mod process;
mod rpc;
mod types;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

pub use agent_tools::{ConcatSession, concat_tools};
pub use backend::{BackendType, ConcatBackend, ReplyImage, ToolReply};
pub use client::{CaptionLine, ClipMove, ConcatClient, Edge, Framing, KeyProperty};
pub use config::{BackendKind, ConcatConfig, ConcatMode, RootsPolicy};
pub use error::ConcatError;
pub use locate::{ENV_VAR as CONCAT_ENV_VAR, locate_binary};
pub use plan::{
    CaptionStyle, CaptionTrack, Command, CropKeyframe, Overlay, PlanFraming, StudioClipPlan,
    TimeRange, Word, apply_plan, plan_from_view, to_concat_commands,
};
pub use process::ProcessBackend;
pub use types::{
    ClipView, ExportJob, ExportPreset, ExportSpec, ExportState, ExportStatus, FrameFile, MediaView,
    ProjectInfo, Summary, VersionInfo, VideoSettings,
};
