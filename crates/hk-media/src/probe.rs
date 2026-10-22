//! `ffprobe -print_format json` wrapper and typed media info.
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{MediaError, Result};
use crate::proc::run_capture;
use crate::tools::Tools;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub index: u32,
    pub kind: StreamKind,
    pub codec: Option<String>,
    pub duration: Option<f64>,
    pub bit_rate: Option<u64>,
    // video
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    /// Clockwise degrees (0/90/180/270) the player applies on display.
    pub rotation: u32,
    // audio
    pub channels: Option<u32>,
    pub sample_rate: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub format_name: Option<String>,
    /// Container duration in seconds.
    pub duration: Option<f64>,
    pub size_bytes: Option<u64>,
    pub bit_rate: Option<u64>,
    pub streams: Vec<StreamInfo>,
}

impl MediaInfo {
    pub fn video(&self) -> Option<&StreamInfo> {
        self.streams.iter().find(|s| s.kind == StreamKind::Video)
    }

    pub fn audio(&self) -> Option<&StreamInfo> {
        self.streams.iter().find(|s| s.kind == StreamKind::Audio)
    }

    pub fn has_audio(&self) -> bool {
        self.audio().is_some()
    }

    /// Frame size as displayed (swapped for 90/270 rotation); what ffmpeg filters see after autorotate.
    pub fn display_size(&self) -> Option<(u32, u32)> {
        let v = self.video()?;
        let (w, h) = (v.width?, v.height?);
        Some(if v.rotation % 180 == 90 {
            (h, w)
        } else {
            (w, h)
        })
    }
}

/// Probe with `HK_FFPROBE` / PATH defaults.
pub async fn probe(path: impl AsRef<Path>) -> Result<MediaInfo> {
    Tools::from_env().probe(path).await
}

impl Tools {
    pub async fn probe(&self, path: impl AsRef<Path>) -> Result<MediaInfo> {
        let args = probe_args(path.as_ref());
        let out = run_capture(&self.ffprobe, &args).await?;
        parse_probe(&out.stdout)
    }
}

pub(crate) fn probe_args(path: &Path) -> Vec<String> {
    [
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .chain(["-i".to_string(), path.display().to_string()])
    .collect()
}

// ffprobe emits most numbers as JSON strings ("duration": "10.000000"), some as numbers.
fn lenient<'de, D, T>(d: D) -> std::result::Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: std::str::FromStr + serde::de::DeserializeOwned,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either<T> {
        Num(T),
        Str(String),
    }
    Ok(match Option::<Either<T>>::deserialize(d)? {
        Some(Either::Num(n)) => Some(n),
        Some(Either::Str(s)) => s.parse().ok(),
        None => None,
    })
}

#[derive(Deserialize)]
struct RawProbe {
    #[serde(default)]
    streams: Vec<RawStream>,
    format: Option<RawFormat>,
}

#[derive(Deserialize)]
struct RawFormat {
    format_name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    duration: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    size: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    bit_rate: Option<u64>,
}

#[derive(Deserialize)]
struct RawStream {
    index: u32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    duration: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    bit_rate: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    channels: Option<u32>,
    #[serde(default, deserialize_with = "lenient")]
    sample_rate: Option<u32>,
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
    #[serde(default)]
    side_data_list: Vec<serde_json::Value>,
}

fn parse_rate(s: &str) -> Option<f64> {
    let (n, d) = s.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d > 0.0 && n > 0.0).then(|| n / d)
}

fn rotation_of(s: &RawStream) -> u32 {
    // Modern ffprobe: displaymatrix side data, counter-clockwise degrees (-90 == rotate 90 cw).
    let side = s
        .side_data_list
        .iter()
        .find_map(|v| v.get("rotation").and_then(serde_json::Value::as_f64));
    // Old files: `rotate` tag, clockwise.
    let deg = match side {
        Some(r) => -r,
        None => s
            .tags
            .get("rotate")
            .and_then(|r| r.parse::<f64>().ok())
            .unwrap_or(0.0),
    };
    // Snap to quarter turns; arbitrary angles are not meaningful for display.
    ((deg / 90.0).round() as i64 * 90).rem_euclid(360) as u32
}

pub(crate) fn parse_probe(json: &str) -> Result<MediaInfo> {
    let raw: RawProbe =
        serde_json::from_str(json).map_err(|e| MediaError::Parse(format!("ffprobe json: {e}")))?;
    let streams = raw
        .streams
        .iter()
        .map(|s| StreamInfo {
            index: s.index,
            kind: match s.codec_type.as_deref() {
                Some("video") => StreamKind::Video,
                Some("audio") => StreamKind::Audio,
                Some("subtitle") => StreamKind::Subtitle,
                _ => StreamKind::Other,
            },
            codec: s.codec_name.clone(),
            duration: s.duration,
            bit_rate: s.bit_rate,
            width: s.width,
            height: s.height,
            fps: s
                .avg_frame_rate
                .as_deref()
                .and_then(parse_rate)
                .or_else(|| s.r_frame_rate.as_deref().and_then(parse_rate)),
            rotation: rotation_of(s),
            channels: s.channels,
            sample_rate: s.sample_rate,
        })
        .collect();
    let f = raw.format;
    Ok(MediaInfo {
        format_name: f.as_ref().and_then(|f| f.format_name.clone()),
        duration: f.as_ref().and_then(|f| f.duration),
        size_bytes: f.as_ref().and_then(|f| f.size),
        bit_rate: f.as_ref().and_then(|f| f.bit_rate),
        streams,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "streams": [
        {"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,
         "avg_frame_rate":"30000/1001","r_frame_rate":"30000/1001","duration":"12.5",
         "side_data_list":[{"side_data_type":"Display Matrix","rotation":-90}]},
        {"index":1,"codec_type":"audio","codec_name":"aac","channels":2,
         "sample_rate":"48000","bit_rate":"128000"}
      ],
      "format": {"format_name":"mov,mp4","duration":"12.512000","size":"1048576","bit_rate":"670000"}
    }"#;

    #[test]
    fn parses_streams_and_format() {
        let m = parse_probe(SAMPLE).unwrap();
        assert_eq!(m.duration, Some(12.512));
        assert_eq!(m.size_bytes, Some(1_048_576));
        let v = m.video().unwrap();
        assert_eq!((v.width, v.height), (Some(1920), Some(1080)));
        assert!((v.fps.unwrap() - 29.97).abs() < 0.01);
        assert_eq!(v.rotation, 90);
        assert_eq!(m.display_size(), Some((1080, 1920)));
        let a = m.audio().unwrap();
        assert_eq!((a.channels, a.sample_rate), (Some(2), Some(48000)));
    }

    #[test]
    fn rotate_tag_and_missing_fields() {
        let m = parse_probe(
            r#"{"streams":[{"index":0,"codec_type":"video","width":10,"height":20,
                "r_frame_rate":"0/0","tags":{"rotate":"270"}}],"format":{}}"#,
        )
        .unwrap();
        let v = m.video().unwrap();
        assert_eq!(v.rotation, 270);
        assert_eq!(v.fps, None);
        assert_eq!(m.duration, None);
        assert!(!m.has_audio());
    }

    #[test]
    fn bad_json_is_parse_error() {
        assert!(matches!(parse_probe("nope"), Err(MediaError::Parse(_))));
    }
}
