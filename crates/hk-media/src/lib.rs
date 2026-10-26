//! ffmpeg/ffprobe process wrapper for Hokkabaz Studio: probe, audio extract/chunking,
//! silence detection, deterministic filtergraph render with progress/cancel, thumbs, proxies.
//!
//! Always spawns with an argv vec, never a shell. Binaries come from `HK_FFMPEG` / `HK_FFPROBE`
//! or `PATH`. On Android no process is spawned: every operation returns [`MediaError::Unsupported`]
//! (graph building and parsers still compile and work) -- render there goes via a remote daemon.
#![cfg_attr(target_os = "android", allow(dead_code))]

mod audio;
mod enhance;
mod error;
mod fmt;
mod graph;
mod probe;
mod proc;
mod render;
mod thumb;
mod tools;

pub use audio::{AudioChunk, AudioSpec, Range, extract_audio, plan_chunks, silencedetect};
pub use enhance::{AudioCleanup, Denoise, HwEncoder, Loudness};
pub use error::{MediaError, Result};
pub use graph::{
    BuiltCommand, CropKey, CropPath, Ducking, Interp, MusicMix, Overlay, OverlayKind, RateControl,
    RenderSpec, Segment, Source, VideoCodec,
};
pub use probe::{MediaInfo, StreamInfo, StreamKind, probe};
pub use render::{Progress, RenderHandle, RenderReport, render};
pub use thumb::{proxy, thumbnail};
pub use tools::{ENV_FFMPEG, ENV_FFPROBE, Tools, managed_dir, resolve_program, set_managed_dir};
