//! Typed views of Concat replies and arguments. Field names follow Concat's camelCase wire format
//! (concat-api `message.rs`, concat-mcp `summary.rs`); everything is `default`-tolerant so a newer
//! Concat adding fields never breaks decoding.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Frame size and rate (`VideoSettings` in concat-api).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSettings {
    pub width: u32,
    pub height: u32,
    pub rate_num: i64,
    pub rate_den: i64,
}

impl VideoSettings {
    /// 9:16 short-form, 30 fps.
    pub const VERTICAL_1080: Self = Self {
        width: 1080,
        height: 1920,
        rate_num: 30,
        rate_den: 1,
    };
    pub const LANDSCAPE_1080: Self = Self {
        width: 1920,
        height: 1080,
        rate_num: 30,
        rate_den: 1,
    };
    pub const SQUARE_1080: Self = Self {
        width: 1080,
        height: 1080,
        rate_num: 30,
        rate_den: 1,
    };

    pub fn aspect(&self) -> f64 {
        f64::from(self.width) / f64::from(self.height.max(1))
    }
}

/// `concat_version` reply, with `mcp.mode` ("window" | "headless") added by the MCP server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VersionInfo {
    pub api_version: String,
    pub concat: String,
    pub capabilities: Vec<String>,
    pub mcp: Value,
}

impl VersionInfo {
    /// `"window"`, `"headless"` (or `None` for an in-process engine / older Concat).
    pub fn mode(&self) -> Option<&str> {
        self.mcp.get("mode").and_then(Value::as_str)
    }
}

/// One entry of `project_list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectInfo {
    pub path: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rate_num: i64,
    pub rate_den: i64,
    pub opened_at: u64,
}

impl Default for ProjectInfo {
    fn default() -> Self {
        Self {
            path: String::new(),
            name: String::new(),
            width: 0,
            height: 0,
            rate_num: 30,
            rate_den: 1,
            opened_at: 0,
        }
    }
}

/// The compact project reading every edit returns (concat-mcp `summary::view`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Summary {
    pub project: ProjectMeta,
    pub can_undo: bool,
    pub can_redo: bool,
    pub timeline: TimelineView,
    pub media: Vec<MediaView>,
    /// Id minted by the last edit (clip, media item, track).
    pub created_id: Option<String>,
}

impl Summary {
    pub fn created(&self, tool: &str) -> Result<&str, crate::ConcatError> {
        self.created_id
            .as_deref()
            .ok_or_else(|| crate::ConcatError::decode(tool, "reply has no `createdId`"))
    }

    pub fn clip(&self, id: &str) -> Option<&ClipView> {
        self.timeline.clips.iter().find(|c| c.id == id)
    }

    pub fn media_item(&self, id: &str) -> Option<&MediaView> {
        self.media.iter().find(|m| m.id == id)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectMeta {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimelineView {
    pub id: String,
    pub name: String,
    /// Seconds.
    pub duration: f64,
    /// Lanes top to bottom.
    pub tracks: Vec<TrackView>,
    /// Clips in time order.
    pub clips: Vec<ClipView>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackView {
    pub id: String,
    pub index: usize,
    pub hidden: bool,
    pub muted: bool,
}

/// A clip as in the summary. Only fields that are not at their default are present upstream, so
/// everything beyond the first block is optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClipView {
    pub id: String,
    /// "video" | "audio" | "image" | "text" | "layer" | "shape".
    pub kind: String,
    pub track: String,
    pub start: f64,
    pub duration: f64,
    pub end: f64,
    pub name: Option<String>,
    /// Media id.
    pub media: Option<String>,
    pub source_start: Option<f64>,
    pub text: Option<String>,
    pub speed: Option<f64>,
    pub offset_y: Option<f64>,
    /// Properties that carry keyframes (values only via `project_get`).
    pub keyframed: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub path: String,
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub has_audio: bool,
}

/// `preview_frame` written to a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameFile {
    pub path: String,
    pub width: u32,
    pub height: u32,
}

/// Export parameters (`ExportSpec` in concat-api; all but `output` optional).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSpec {
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crf: Option<u8>,
    /// x264 speed preset ("ultrafast".."veryslow"), not a Hokkabaz preset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_num: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_den: Option<i64>,
    /// "h264" | "hevc" | "av1".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ten_bit: Option<bool>,
}

/// Ready-made export settings for the Studio screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportPreset {
    /// Fast, small, 720p-ish check render.
    Draft,
    /// 1080x1920 H.264 for Shorts/Reels/TikTok.
    Vertical1080,
    /// 1920x1080 H.264.
    Landscape1080,
    /// Timeline size, HEVC, crf 18, slow.
    Master,
}

impl ExportPreset {
    pub fn spec(self, output: impl Into<String>) -> ExportSpec {
        let base = ExportSpec {
            output: output.into(),
            ..ExportSpec::default()
        };
        match self {
            Self::Draft => ExportSpec {
                crf: Some(30),
                preset: Some("veryfast".into()),
                width: Some(720),
                height: Some(1280),
                codec: Some("h264".into()),
                ..base
            },
            Self::Vertical1080 => ExportSpec {
                crf: Some(21),
                preset: Some("medium".into()),
                width: Some(1080),
                height: Some(1920),
                rate_num: Some(30),
                rate_den: Some(1),
                codec: Some("h264".into()),
                ..base
            },
            Self::Landscape1080 => ExportSpec {
                crf: Some(21),
                preset: Some("medium".into()),
                width: Some(1920),
                height: Some(1080),
                codec: Some("h264".into()),
                ..base
            },
            Self::Master => ExportSpec {
                crf: Some(18),
                preset: Some("slow".into()),
                codec: Some("hevc".into()),
                ..base
            },
        }
    }
}

/// `export_start` reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportJob {
    pub job: String,
    pub path: String,
    pub output: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportState {
    #[default]
    Running,
    Done,
    Cancelled,
    Failed,
}

impl ExportState {
    pub fn is_terminal(self) -> bool {
        self != Self::Running
    }
}

/// `export_status` reply.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExportStatus {
    pub job: String,
    pub path: String,
    pub output: String,
    pub state: ExportState,
    /// "cutout" | "stabilize" | "video" | "audio" | "mux".
    pub stage: Option<String>,
    pub frame: i64,
    pub total: i64,
    /// 0..=1 within the current stage.
    pub fraction: f64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub error: Option<Value>,
}

/// Stages in order, for one progress figure (same table as Concat's `export_wait`).
const STAGES: [&str; 5] = ["cutout", "stabilize", "video", "audio", "mux"];

impl ExportStatus {
    /// Progress across all stages, 0..=1; 1.0 only when done.
    pub fn overall(&self) -> f64 {
        match self.state {
            ExportState::Done => 1.0,
            ExportState::Running => {
                let place = self
                    .stage
                    .as_deref()
                    .and_then(|s| STAGES.iter().position(|k| *k == s))
                    .unwrap_or(0) as f64;
                ((place + self.fraction.clamp(0.0, 1.0)) / STAGES.len() as f64).min(0.999)
            }
            _ => 0.0,
        }
    }
}
