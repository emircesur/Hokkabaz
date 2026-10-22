//! Studio EDL <-> Concat project adapter.
//!
//! [`StudioClipPlan`] is what Hokkabaz Studio decides (kept source ranges, transcript words +
//! caption style, reframe path, overlays). [`to_concat_commands`] lowers it to a flat list of
//! [`Command`]s; [`apply_commands`] runs them against a [`ConcatClient`] (a few commands need ids
//! minted by earlier calls, so they cannot be one static Concat batch). [`plan_from_view`] goes
//! back from a `project_get` reply.
//!
//! Semantics relied on (all from Concat's `commands/mod.rs`, not yet exercised against a real
//! binary here): `clip_add` places the whole media at `start`; a head `clip_trim` moves the
//! in-point and the clip start by the same delta; `clip_trim` end with negative delta shortens;
//! `clips_move` repositions; a video clip carries its own audio.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::client::{CaptionLine, ConcatClient, Framing};
use crate::types::{MediaView, VideoSettings};
use crate::ConcatError;

const EPS: f64 = 1e-4;
/// Text clips placed at or below this vertical offset (frame fractions, positive = down) are read
/// back as captions, others as titles. Concat does not tag the two.
pub const CAPTION_MIN_OFFSET_Y: f64 = 0.15;
const DEFAULT_CAPTION_OFFSET_Y: f64 = 0.3;
const MIN_LINE_SECS: f64 = 0.15;

// -------------------------------------------------------------------------- plan types

/// Half-open source range in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

impl TimeRange {
    pub fn len(&self) -> f64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.len() <= 0.0
    }
}

/// One transcript word, source-time seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

fn d_max_words() -> usize {
    4
}
fn d_max_chars() -> usize {
    28
}
fn d_max_gap() -> f64 {
    0.7
}

/// Caption look + line breaking. Unset look fields fall back to Concat's own caption defaults
/// (Hanken Grotesk 600, 0.04 of frame height). Units are Concat's `TextStyle` units.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptionStyle {
    pub font_family: Option<String>,
    /// Fraction of frame height.
    pub font_size: Option<f64>,
    pub font_weight: Option<f64>,
    /// "#rrggbb".
    pub color: Option<String>,
    pub stroke_color: Option<String>,
    pub stroke_width: Option<f64>,
    pub background: Option<String>,
    pub uppercase: bool,
    /// Vertical placement, fraction of frame height from centre, positive down. Default 0.3.
    pub offset_y: Option<f64>,
    #[serde(default = "d_max_words")]
    pub max_words_per_line: usize,
    #[serde(default = "d_max_chars")]
    pub max_chars_per_line: usize,
    /// A silence longer than this starts a new line.
    #[serde(default = "d_max_gap")]
    pub max_gap_secs: f64,
}

impl Default for CaptionStyle {
    fn default() -> Self {
        Self {
            font_family: None,
            font_size: None,
            font_weight: None,
            color: None,
            stroke_color: None,
            stroke_width: None,
            background: None,
            uppercase: false,
            offset_y: None,
            max_words_per_line: d_max_words(),
            max_chars_per_line: d_max_chars(),
            max_gap_secs: d_max_gap(),
        }
    }
}

impl CaptionStyle {
    /// Concat `TextStyle` JSON with only the fields that are set.
    pub fn text_style(&self) -> Value {
        let mut m = Map::new();
        let mut put = |k: &str, v: Option<Value>| {
            if let Some(v) = v {
                m.insert(k.to_owned(), v);
            }
        };
        put("fontFamily", self.font_family.clone().map(Value::from));
        put("fontSize", self.font_size.map(Value::from));
        put("fontWeight", self.font_weight.map(Value::from));
        put("color", self.color.clone().map(Value::from));
        put("strokeColor", self.stroke_color.clone().map(Value::from));
        put("strokeWidth", self.stroke_width.map(Value::from));
        put("background", self.background.clone().map(Value::from));
        Value::Object(m)
    }

    pub fn offset(&self) -> f64 {
        self.offset_y.unwrap_or(DEFAULT_CAPTION_OFFSET_Y)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CaptionTrack {
    /// Source-time words (e.g. from `hk-stt`); words outside the kept ranges are dropped.
    pub words: Vec<Word>,
    #[serde(default)]
    pub style: CaptionStyle,
}

/// Reframe path point. `time` is seconds on the EDITED (output) timeline. `center_*` is the
/// subject position as a fraction of the source frame (0..1); `zoom` >= 1 zooms in beyond "fill".
/// Linearly interpolated between points, held outside them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CropKeyframe {
    pub time: f64,
    pub center_x: f64,
    pub center_y: f64,
    #[serde(default = "one")]
    pub zoom: f64,
}

fn one() -> f64 {
    1.0
}

/// B-roll / titles on top of the main cut. `start` is on the OUTPUT timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Overlay {
    Title {
        text: String,
        start: f64,
        duration: f64,
        /// Concat `TextStyle` fields (camelCase), only those that differ from default.
        #[serde(default)]
        style: Option<Value>,
        #[serde(default)]
        offset_y: Option<f64>,
    },
    Media {
        file: String,
        start: f64,
        duration: f64,
        #[serde(default = "one")]
        scale: f64,
        #[serde(default)]
        offset_x: f64,
        #[serde(default)]
        offset_y: f64,
        #[serde(default = "one")]
        opacity: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanFraming {
    /// Leave the picture as Concat places it.
    Keep,
    Fit,
    /// Cover the frame (9:16 from 16:9); the default.
    #[default]
    Fill,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudioClipPlan {
    pub name: String,
    /// Absolute path of the source media.
    pub source: String,
    /// Source ranges to keep, in output order (ascending; touching ranges are merged).
    pub kept_ranges: Vec<TimeRange>,
    #[serde(default)]
    pub captions: Option<CaptionTrack>,
    #[serde(default)]
    pub crop_keyframes: Vec<CropKeyframe>,
    #[serde(default)]
    pub overlays: Vec<Overlay>,
    #[serde(default = "vertical")]
    pub video: VideoSettings,
    #[serde(default)]
    pub framing: PlanFraming,
}

fn vertical() -> VideoSettings {
    VideoSettings::VERTICAL_1080
}

impl StudioClipPlan {
    pub fn new(name: impl Into<String>, source: impl Into<String>, kept: Vec<TimeRange>) -> Self {
        Self {
            name: name.into(),
            source: source.into(),
            kept_ranges: kept,
            captions: None,
            crop_keyframes: Vec::new(),
            overlays: Vec::new(),
            video: vertical(),
            framing: PlanFraming::Fill,
        }
    }

    /// Output length in seconds.
    pub fn output_duration(&self) -> f64 {
        self.kept_ranges.iter().map(TimeRange::len).sum()
    }
}

// -------------------------------------------------------------------------- commands

/// Plan lowered to Concat operations. Keys (`media`, `range`) are resolved to Concat ids by
/// [`apply_commands`]; nothing here needs a running Concat to build or test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// `media_import`; `key` names the bin item for later commands.
    ImportMedia { key: String, file: String },
    /// Source `[source_start, source_end)` of `media` at output time `at`; becomes clip `range`.
    PlaceRange { range: usize, media: String, source_start: f64, source_end: f64, at: f64 },
    /// `clip_frame` fit/fill on a placed range.
    Frame { range: usize, framing: PlanFraming },
    /// Reframe keys for one range (fractions of that clip), resolved to scale/offset keyframes.
    CropKeys { range: usize, keys: Vec<RangeKey> },
    AddCaptions { lines: Vec<CaptionLine>, style: Value, offset_y: f64 },
    AddTitle { text: String, start: f64, duration: f64, style: Option<Value>, offset_y: Option<f64> },
    AddMediaOverlay {
        media: String,
        start: f64,
        duration: f64,
        scale: f64,
        offset_x: f64,
        offset_y: f64,
        opacity: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RangeKey {
    /// Fraction of the clip, 0..=1.
    pub at: f64,
    pub center_x: f64,
    pub center_y: f64,
    pub zoom: f64,
}

/// Validates and merges ranges: ascending, finite, positive length; touching/overlapping merge.
pub fn normalize_ranges(ranges: &[TimeRange]) -> Result<Vec<TimeRange>, ConcatError> {
    if ranges.is_empty() {
        return Err(ConcatError::InvalidPlan("kept_ranges is empty".into()));
    }
    let mut out: Vec<TimeRange> = Vec::with_capacity(ranges.len());
    for r in ranges {
        if !(r.start.is_finite() && r.end.is_finite() && r.start >= 0.0 && r.end - r.start > EPS) {
            return Err(ConcatError::InvalidPlan(format!("bad kept range [{}, {})", r.start, r.end)));
        }
        match out.last_mut() {
            Some(last) if r.start < last.start => {
                return Err(ConcatError::InvalidPlan("kept_ranges must be ascending".into()));
            }
            Some(last) if r.start <= last.end + EPS => last.end = last.end.max(r.end),
            _ => out.push(*r),
        }
    }
    Ok(out)
}

/// Output-timeline start of each normalised range.
fn output_starts(ranges: &[TimeRange]) -> Vec<f64> {
    let mut at = 0.0;
    ranges
        .iter()
        .map(|r| {
            let s = at;
            at += r.len();
            s
        })
        .collect()
}

/// A word on the output timeline, with the kept range it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub range: usize,
}

/// Maps source-time words onto the output timeline. A word is kept when its midpoint lies in a
/// kept range; its times are clamped into that range.
pub fn map_words(words: &[Word], ranges: &[TimeRange]) -> Vec<TimedWord> {
    let starts = output_starts(ranges);
    let mut out = Vec::new();
    for w in words {
        let text = w.text.trim();
        if text.is_empty() || !(w.start.is_finite() && w.end.is_finite()) {
            continue;
        }
        let mid = (w.start + w.end) / 2.0;
        let Some(i) = ranges.iter().position(|r| mid >= r.start && mid < r.end) else { continue };
        let r = ranges[i];
        out.push(TimedWord {
            text: text.to_owned(),
            start: starts[i] + w.start.clamp(r.start, r.end) - r.start,
            end: starts[i] + w.end.clamp(r.start, r.end) - r.start,
            range: i,
        });
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// Groups timed words into caption lines (never across a cut, a long gap, or the style limits).
pub fn group_words(words: &[TimedWord], style: &CaptionStyle) -> Vec<CaptionLine> {
    let mut lines: Vec<CaptionLine> = Vec::new();
    let mut cur: Option<(CaptionLine, usize, usize, f64)> = None; // line, words, chars, last end
    let max_words = style.max_words_per_line.max(1);
    let max_chars = style.max_chars_per_line.max(1);
    for (i, w) in words.iter().enumerate() {
        let text = if style.uppercase { w.text.to_uppercase() } else { w.text.clone() };
        let wchars = text.chars().count();
        let breaks = match &cur {
            None => false,
            Some((_, n, chars, last_end)) => {
                *n >= max_words
                    || chars + 1 + wchars > max_chars
                    || w.start - last_end > style.max_gap_secs
                    || words[i - 1].range != w.range
            }
        };
        if breaks && let Some((line, ..)) = cur.take() {
            lines.push(line);
        }
        match &mut cur {
            None => {
                cur = Some((CaptionLine { start: w.start, end: w.end, text }, 1, wchars, w.end));
            }
            Some((line, n, chars, last_end)) => {
                line.text.push(' ');
                line.text.push_str(&text);
                line.end = line.end.max(w.end);
                *n += 1;
                *chars += 1 + wchars;
                *last_end = w.end;
            }
        }
    }
    lines.extend(cur.map(|(l, ..)| l));
    // Lines must not overlap, and each needs a visible duration.
    for i in 0..lines.len() {
        if let Some(next) = lines.get(i + 1).map(|n| n.start) {
            lines[i].end = lines[i].end.min(next);
        }
        if lines[i].end - lines[i].start < MIN_LINE_SECS {
            let cap = lines.get(i + 1).map_or(f64::INFINITY, |n| n.start);
            lines[i].end = (lines[i].start + MIN_LINE_SECS).min(cap);
        }
    }
    lines.retain(|l| l.end - l.start > EPS);
    lines
}

// ------------------------------------------------------------------------ reframe math

/// `(scale, offset_x, offset_y)` in Concat's clip units that centre source point
/// `(center_x, center_y)` (fractions of the source frame) in the frame, zoomed by `zoom` beyond
/// "fill". Scale 1 = picture fitted inside the frame (Concat `framing_scale`); offsets are frame
/// fractions from centre, positive right/down; clamped so the picture never uncovers the frame.
pub fn crop_to_transform(
    source: (f64, f64),
    frame: (f64, f64),
    center_x: f64,
    center_y: f64,
    zoom: f64,
) -> Option<(f64, f64, f64)> {
    let (g, f) = fit_geometry(source, frame)?;
    let scale = (g.fill * zoom.max(0.05)).clamp(0.05, 8.0);
    let (wfrac, hfrac) = (g.fit_w * scale / f.0, g.fit_h * scale / f.1);
    let lim = |frac: f64| ((frac - 1.0) / 2.0).max(0.0);
    let ox = ((0.5 - center_x.clamp(0.0, 1.0)) * wfrac).clamp(-lim(wfrac), lim(wfrac));
    let oy = ((0.5 - center_y.clamp(0.0, 1.0)) * hfrac).clamp(-lim(hfrac), lim(hfrac));
    Some((scale, ox, oy))
}

/// Inverse of [`crop_to_transform`]: `(center_x, center_y, zoom)`.
pub fn transform_to_crop(source: (f64, f64), frame: (f64, f64), scale: f64, ox: f64, oy: f64) -> Option<(f64, f64, f64)> {
    let (g, f) = fit_geometry(source, frame)?;
    let (wfrac, hfrac) = (g.fit_w * scale / f.0, g.fit_h * scale / f.1);
    Some((0.5 - ox / wfrac, 0.5 - oy / hfrac, scale / g.fill))
}

struct Fit {
    fit_w: f64,
    fit_h: f64,
    fill: f64,
}

fn fit_geometry(source: (f64, f64), frame: (f64, f64)) -> Option<(Fit, (f64, f64))> {
    let ((sw, sh), (fw, fh)) = (source, frame);
    if !(sw > 0.0 && sh > 0.0 && fw > 0.0 && fh > 0.0) {
        return None;
    }
    let fit = (fw / sw).min(fh / sh);
    let (w, h) = (sw * fit, sh * fit);
    Some((Fit { fit_w: w, fit_h: h, fill: (fw / w).max(fh / h) }, (fw, fh)))
}

/// Linear interpolation of the reframe path at output time `t`; held before/after.
fn sample_path(keys: &[CropKeyframe], t: f64) -> (f64, f64, f64) {
    let (first, last) = (keys[0], keys[keys.len() - 1]);
    if t <= first.time {
        return (first.center_x, first.center_y, first.zoom);
    }
    if t >= last.time {
        return (last.center_x, last.center_y, last.zoom);
    }
    let i = keys.iter().rposition(|k| k.time <= t).unwrap_or(0);
    let (a, b) = (keys[i], keys[i + 1]);
    let u = if b.time - a.time > EPS { (t - a.time) / (b.time - a.time) } else { 0.0 };
    let l = |x: f64, y: f64| x + (y - x) * u;
    (l(a.center_x, b.center_x), l(a.center_y, b.center_y), l(a.zoom, b.zoom))
}

fn range_keys(path: &[CropKeyframe], out_start: f64, len: f64) -> Vec<RangeKey> {
    let at = |t: f64| ((t - out_start) / len).clamp(0.0, 1.0);
    let mut keys: Vec<RangeKey> = Vec::new();
    let mut push = |a: f64, v: (f64, f64, f64)| {
        if !keys.iter().any(|k| (k.at - a).abs() < 0.002) {
            keys.push(RangeKey { at: a, center_x: v.0, center_y: v.1, zoom: v.2 });
        }
    };
    push(0.0, sample_path(path, out_start));
    for k in path.iter().filter(|k| k.time > out_start + EPS && k.time < out_start + len - EPS) {
        push(at(k.time), (k.center_x, k.center_y, k.zoom));
    }
    push(1.0, sample_path(path, out_start + len));
    keys.sort_by(|a, b| a.at.total_cmp(&b.at));
    // Constant path inside this range: one key is enough.
    let c = keys[0];
    if keys.iter().all(|k| (k.center_x - c.center_x).abs() < 1e-6 && (k.center_y - c.center_y).abs() < 1e-6 && (k.zoom - c.zoom).abs() < 1e-6) {
        keys.truncate(1);
    }
    keys
}

// ----------------------------------------------------------------------------- lowering

/// Lowers a plan to Concat operations. Pure; validates the plan.
pub fn to_concat_commands(plan: &StudioClipPlan) -> Result<Vec<Command>, ConcatError> {
    if plan.source.trim().is_empty() {
        return Err(ConcatError::InvalidPlan("source is empty".into()));
    }
    if plan.video.width == 0 || plan.video.height == 0 {
        return Err(ConcatError::InvalidPlan("video size is zero".into()));
    }
    let ranges = normalize_ranges(&plan.kept_ranges)?;
    let starts = output_starts(&ranges);
    let total: f64 = ranges.iter().map(TimeRange::len).sum();

    let mut cmds = vec![Command::ImportMedia { key: "source".into(), file: plan.source.clone() }];
    for (i, o) in plan.overlays.iter().enumerate() {
        if let Overlay::Media { file, start, duration, .. } = o {
            check_span(*start, *duration)?;
            cmds.push(Command::ImportMedia { key: overlay_key(i), file: file.clone() });
        }
    }
    for (i, r) in ranges.iter().enumerate() {
        cmds.push(Command::PlaceRange {
            range: i,
            media: "source".into(),
            source_start: r.start,
            source_end: r.end,
            at: starts[i],
        });
    }

    let mut path = plan.crop_keyframes.clone();
    for k in &path {
        if !(k.time.is_finite() && k.center_x.is_finite() && k.center_y.is_finite() && k.zoom.is_finite() && k.zoom > 0.0) {
            return Err(ConcatError::InvalidPlan("crop keyframe has a non-finite or non-positive value".into()));
        }
    }
    path.sort_by(|a, b| a.time.total_cmp(&b.time));
    if path.is_empty() {
        if plan.framing != PlanFraming::Keep {
            cmds.extend((0..ranges.len()).map(|range| Command::Frame { range, framing: plan.framing }));
        }
    } else {
        for (i, r) in ranges.iter().enumerate() {
            cmds.push(Command::CropKeys { range: i, keys: range_keys(&path, starts[i], r.len()) });
        }
    }

    if let Some(track) = &plan.captions {
        let lines = group_words(&map_words(&track.words, &ranges), &track.style);
        if !lines.is_empty() {
            cmds.push(Command::AddCaptions { lines, style: track.style.text_style(), offset_y: track.style.offset() });
        }
    }
    for (i, o) in plan.overlays.iter().enumerate() {
        match o {
            Overlay::Title { text, start, duration, style, offset_y } => {
                check_span(*start, *duration)?;
                cmds.push(Command::AddTitle {
                    text: text.clone(),
                    start: *start,
                    duration: *duration,
                    style: style.clone(),
                    offset_y: *offset_y,
                });
            }
            Overlay::Media { start, duration, scale, offset_x, offset_y, opacity, .. } => cmds.push(Command::AddMediaOverlay {
                media: overlay_key(i),
                start: *start,
                duration: *duration,
                scale: *scale,
                offset_x: *offset_x,
                offset_y: *offset_y,
                opacity: *opacity,
            }),
        }
    }
    let _ = total;
    Ok(cmds)
}

fn overlay_key(i: usize) -> String {
    format!("overlay:{i}")
}

fn check_span(start: f64, duration: f64) -> Result<(), ConcatError> {
    if start.is_finite() && duration.is_finite() && start >= 0.0 && duration > EPS {
        Ok(())
    } else {
        Err(ConcatError::InvalidPlan(format!("bad overlay span start={start} duration={duration}")))
    }
}

// ------------------------------------------------------------------------------ applying

/// What [`apply_commands`] built.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedPlan {
    pub project: String,
    /// Concat clip id per kept range.
    pub range_clips: Vec<(usize, String)>,
    pub summary: crate::types::Summary,
}

struct MediaState {
    id: String,
    duration: Option<f64>,
    size: Option<(f64, f64)>,
}

/// Creates `location/name` (video settings from the plan) and applies the plan. Returns the path.
pub async fn apply_plan(client: &ConcatClient, location: &str, plan: &StudioClipPlan) -> Result<AppliedPlan, ConcatError> {
    let cmds = to_concat_commands(plan)?;
    client.project_create(location, &plan.name, Some(plan.video)).await?;
    let project = format!("{}/{}", location.trim_end_matches(['/', '\\']), plan.name);
    apply_commands(client, &project, plan.video, &cmds).await
}

/// Runs `cmds` on an already open project.
pub async fn apply_commands(
    client: &ConcatClient,
    project: &str,
    video: VideoSettings,
    cmds: &[Command],
) -> Result<AppliedPlan, ConcatError> {
    let mut media: HashMap<String, MediaState> = HashMap::new();
    let mut clips: HashMap<usize, String> = HashMap::new();
    let mut placed: Vec<(usize, String)> = Vec::new();
    let frame = (f64::from(video.width), f64::from(video.height));

    let read = client.timeline_read(project).await?;
    let main_track = match read.timeline.tracks.last() {
        Some(t) => t.id.clone(),
        None => client.add_track(project, true).await?,
    };

    let find = |media: &HashMap<String, MediaState>, key: &str| -> Result<(String, Option<f64>), ConcatError> {
        media
            .get(key)
            .map(|m| (m.id.clone(), m.duration))
            .ok_or_else(|| ConcatError::InvalidPlan(format!("command uses media `{key}` before it is imported")))
    };
    let need_len = |d: Option<f64>, key: &str| d.ok_or_else(|| ConcatError::decode("media_import", format!("media `{key}` has no duration")));

    for c in cmds {
        match c {
            Command::ImportMedia { key, file } => {
                let id = client.import_media(project, file).await?;
                let s = client.timeline_read(project).await?;
                let m: Option<&MediaView> = s.media_item(&id);
                media.insert(
                    key.clone(),
                    MediaState {
                        id,
                        duration: m.and_then(|m| m.duration),
                        size: m.and_then(|m| Some((f64::from(m.width?), f64::from(m.height?)))),
                    },
                );
            }
            Command::PlaceRange { range, media: key, source_start, source_end, at } => {
                let (mid, dur) = find(&media, key)?;
                let len = need_len(dur, key)?;
                if *source_end > len + 0.05 {
                    return Err(ConcatError::InvalidPlan(format!(
                        "kept range ends at {source_end}s but the source is only {len}s long"
                    )));
                }
                let clip = client.add_clip(project, &mid, &main_track, *at, false).await?;
                let mut ops: Vec<Value> = Vec::new();
                if *source_start > EPS {
                    ops.push(json!({ "op": "trimClip", "clipId": clip, "edge": "start", "delta": source_start, "ripple": false }));
                }
                let tail = len - source_end;
                if tail > EPS {
                    ops.push(json!({ "op": "trimClip", "clipId": clip, "edge": "end", "delta": -tail, "ripple": false }));
                }
                if *source_start > EPS {
                    // Head trim keeps the picture where it was, so the clip now starts at at+start.
                    ops.push(json!({ "op": "moveClips", "moves": [{ "clipId": clip, "start": at, "trackId": main_track }] }));
                }
                if !ops.is_empty() {
                    client.edit_apply(project, json!({ "op": "batch", "commands": ops })).await?;
                }
                clips.insert(*range, clip.clone());
                placed.push((*range, clip));
            }
            Command::Frame { range, framing } => {
                let clip = range_clip(&clips, *range)?;
                let f = match framing {
                    PlanFraming::Keep => continue,
                    PlanFraming::Fit => Framing::Fit,
                    PlanFraming::Fill => Framing::Fill,
                };
                client.frame_clip(project, clip, f, 0.0).await?;
            }
            Command::CropKeys { range, keys } => {
                let clip = range_clip(&clips, *range)?;
                let size = media
                    .get("source")
                    .and_then(|m| m.size)
                    .ok_or_else(|| ConcatError::decode("media_import", "source media has no frame size, cannot reframe"))?;
                let mut ops = Vec::new();
                for k in keys {
                    let (s, ox, oy) = crop_to_transform(size, frame, k.center_x, k.center_y, k.zoom)
                        .ok_or_else(|| ConcatError::InvalidPlan("cannot compute reframe from zero sizes".into()))?;
                    for (prop, v) in [("scale", s), ("offsetX", ox), ("offsetY", oy)] {
                        ops.push(json!({ "op": "setClipKey", "clipId": clip, "property": prop, "at": k.at, "value": v }));
                    }
                }
                client.edit_apply(project, json!({ "op": "batch", "commands": ops })).await?;
            }
            Command::AddCaptions { lines, style, offset_y } => {
                client.add_captions(project, lines, Some(style.clone()), Some(*offset_y)).await?;
            }
            Command::AddTitle { text, start, duration, style, offset_y } => {
                client.add_title(project, text, *start, *duration, style.clone(), *offset_y).await?;
            }
            Command::AddMediaOverlay { media: key, start, duration, scale, offset_x, offset_y, opacity } => {
                let (mid, dur) = find(&media, key)?;
                let len = need_len(dur, key)?;
                let track = client.add_track(project, false).await?;
                let clip = client.add_clip(project, &mid, &track, *start, false).await?;
                let mut ops = Vec::new();
                if len - duration > EPS {
                    ops.push(json!({ "op": "trimClip", "clipId": clip, "edge": "end", "delta": -(len - duration), "ripple": false }));
                }
                ops.push(json!({ "op": "updateClip", "clipId": clip, "patch": { "opacity": opacity } }));
                ops.push(json!({
                    "op": "setClipTransform", "clipId": clip,
                    "scale": scale, "offsetX": offset_x, "offsetY": offset_y, "rotation": null,
                }));
                client.edit_apply(project, json!({ "op": "batch", "commands": ops })).await?;
            }
        }
    }
    let summary = client.timeline_read(project).await?;
    placed.sort_by_key(|(r, _)| *r);
    Ok(AppliedPlan { project: project.to_owned(), range_clips: placed, summary })
}

fn range_clip(clips: &HashMap<usize, String>, range: usize) -> Result<&String, ConcatError> {
    clips
        .get(&range)
        .ok_or_else(|| ConcatError::InvalidPlan(format!("range {range} used before it was placed")))
}

// -------------------------------------------------------------------------- reading back

/// Rebuilds a plan from a `project_get` reply (the full editor view; `timeline_read` lacks
/// keyframe values and text styles). `source` selects the main media by path; `None` takes the
/// media of the first video clip.
///
/// Lossy by nature: caption word timings are spread evenly across each line; text clips are
/// split into captions/titles by [`CAPTION_MIN_OFFSET_Y`]; effects, transitions and speed are
/// not carried by the plan.
pub fn plan_from_view(view: &Value, source: Option<&str>) -> Result<StudioClipPlan, ConcatError> {
    let bad = |why: &str| ConcatError::decode("project_get", why);
    let project = view.get("project").ok_or_else(|| bad("no `project`"))?;
    let timelines = project.get("timelines").and_then(Value::as_array).ok_or_else(|| bad("no timelines"))?;
    let active = project.get("activeTimelineId").and_then(Value::as_str);
    let tl = timelines
        .iter()
        .find(|t| t.get("id").and_then(Value::as_str) == active)
        .or_else(|| timelines.first())
        .ok_or_else(|| bad("no timeline"))?;
    let media_path: HashMap<&str, &Value> = project
        .get("media")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| Some((m.get("id")?.as_str()?, m)))
        .collect();
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64);
    let mut clips: Vec<&Value> = tl.get("clips").and_then(Value::as_array).into_iter().flatten().collect();
    clips.sort_by(|a, b| num(a, "start").unwrap_or(0.0).total_cmp(&num(b, "start").unwrap_or(0.0)));

    let path_of = |c: &Value| -> Option<&str> { media_path.get(c.get("mediaId")?.as_str()?)?.get("path")?.as_str() };
    let kind = |c: &Value| c.get("kind").and_then(Value::as_str).unwrap_or("").to_owned();
    let source_path: String = match source {
        Some(s) => s.to_owned(),
        None => clips
            .iter()
            .find(|c| kind(c) == "video")
            .and_then(|c| path_of(c))
            .ok_or_else(|| bad("no video clip"))?
            .to_owned(),
    };

    let settings = view.get("settings");
    let video = VideoSettings {
        width: settings.and_then(|s| num(s, "width")).unwrap_or(1080.0) as u32,
        height: settings.and_then(|s| num(s, "height")).unwrap_or(1920.0) as u32,
        rate_num: settings.and_then(|s| num(s, "rateNum")).unwrap_or(30.0) as i64,
        rate_den: settings.and_then(|s| num(s, "rateDen")).unwrap_or(1.0) as i64,
    };
    let frame = (f64::from(video.width), f64::from(video.height));

    let mains: Vec<&Value> = clips.iter().copied().filter(|c| kind(c) == "video" && path_of(c) == Some(source_path.as_str())).collect();
    if mains.is_empty() {
        return Err(bad("no clips of the source media"));
    }
    let src_size = mains
        .first()
        .and_then(|c| media_path.get(c.get("mediaId")?.as_str()?))
        .and_then(|m| Some((num(m, "width")?, num(m, "height")?)));

    // Ranges + reframe path. Each main clip is one kept range; consecutive clips that continue the
    // source seamlessly are one range (a split that was never cut).
    let mut kept: Vec<TimeRange> = Vec::new();
    let mut out_starts: Vec<f64> = Vec::new();
    let mut path: Vec<CropKeyframe> = Vec::new();
    for c in &mains {
        let (start, dur) = (num(c, "start").unwrap_or(0.0), num(c, "duration").unwrap_or(0.0));
        let (ss, speed) = (num(c, "sourceStart").unwrap_or(0.0), num(c, "speed").unwrap_or(1.0));
        let r = TimeRange { start: ss, end: ss + dur * speed };
        out_starts.push(start);
        match kept.last_mut() {
            Some(last) if (last.end - r.start).abs() < 1e-3 => last.end = r.end,
            _ => kept.push(r),
        }
        if let (Some(keys), Some(size)) = (c.get("keys").and_then(Value::as_array), src_size) {
            path.extend(read_path(keys, start, dur, size, frame, num(c, "scale"), num(c, "offsetX"), num(c, "offsetY")));
        }
    }
    path.sort_by(|a, b| a.time.total_cmp(&b.time));
    path.dedup_by(|b, a| (a.time - b.time).abs() < 1e-3);
    let ranges_out = output_starts(&kept);

    let mut captions: Vec<(CaptionLine, f64)> = Vec::new();
    let mut cap_style = CaptionStyle::default();
    let mut overlays = Vec::new();
    for c in &clips {
        let (start, dur) = (num(c, "start").unwrap_or(0.0), num(c, "duration").unwrap_or(0.0));
        match kind(c).as_str() {
            "text" => {
                let text = c.pointer("/text/content").and_then(Value::as_str).unwrap_or("").to_owned();
                let oy = num(c, "offsetY").unwrap_or(0.0);
                if oy >= CAPTION_MIN_OFFSET_Y {
                    if captions.is_empty() {
                        cap_style = style_from_text(c.get("text"), oy);
                    }
                    captions.push((CaptionLine { start, end: start + dur, text }, oy));
                } else {
                    overlays.push(Overlay::Title { text, start, duration: dur, style: None, offset_y: (oy != 0.0).then_some(oy) });
                }
            }
            "video" | "image" if path_of(c) != Some(source_path.as_str()) => {
                if let Some(file) = path_of(c) {
                    overlays.push(Overlay::Media {
                        file: file.to_owned(),
                        start,
                        duration: dur,
                        scale: num(c, "scale").unwrap_or(1.0),
                        offset_x: num(c, "offsetX").unwrap_or(0.0),
                        offset_y: num(c, "offsetY").unwrap_or(0.0),
                        opacity: num(c, "opacity").unwrap_or(1.0),
                    });
                }
            }
            _ => {}
        }
    }
    let words: Vec<Word> = captions
        .iter()
        .flat_map(|(l, _)| line_to_words(l, &kept, &ranges_out))
        .collect();
    let mut plan = StudioClipPlan::new(
        settings.and_then(|s| s.get("name")).and_then(Value::as_str).unwrap_or("clip"),
        source_path,
        kept,
    );
    plan.video = video;
    plan.crop_keyframes = path;
    plan.overlays = overlays;
    if !words.is_empty() {
        plan.captions = Some(CaptionTrack { words, style: cap_style });
    }
    Ok(plan)
}

#[allow(clippy::too_many_arguments)]
fn read_path(
    keys: &[Value],
    clip_start: f64,
    clip_dur: f64,
    size: (f64, f64),
    frame: (f64, f64),
    scale0: Option<f64>,
    ox0: Option<f64>,
    oy0: Option<f64>,
) -> Vec<CropKeyframe> {
    let series = |name: &str| -> Vec<(f64, f64)> {
        let mut v: Vec<(f64, f64)> = keys
            .iter()
            .filter(|k| k.get("property").and_then(Value::as_str) == Some(name))
            .filter_map(|k| Some((k.get("at")?.as_f64()?, k.get("value")?.as_f64()?)))
            .collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v
    };
    let (s, x, y) = (series("scale"), series("offsetX"), series("offsetY"));
    let mut ats: Vec<f64> = s.iter().chain(&x).chain(&y).map(|p| p.0).collect();
    ats.sort_by(f64::total_cmp);
    ats.dedup_by(|a, b| (*a - *b).abs() < 0.002);
    let at_value = |v: &[(f64, f64)], at: f64, default: f64| -> f64 {
        match v {
            [] => default,
            [only] => only.1,
            _ if at <= v[0].0 => v[0].1,
            _ if at >= v[v.len() - 1].0 => v[v.len() - 1].1,
            _ => {
                let i = v.iter().rposition(|p| p.0 <= at).unwrap_or(0);
                let (a, b) = (v[i], v[i + 1]);
                a.1 + (b.1 - a.1) * ((at - a.0) / (b.0 - a.0).max(1e-9))
            }
        }
    };
    ats.into_iter()
        .filter_map(|at| {
            let (c, d, z) = transform_to_crop(
                size,
                frame,
                at_value(&s, at, scale0.unwrap_or(1.0)),
                at_value(&x, at, ox0.unwrap_or(0.0)),
                at_value(&y, at, oy0.unwrap_or(0.0)),
            )?;
            Some(CropKeyframe { time: clip_start + at * clip_dur, center_x: c, center_y: d, zoom: z })
        })
        .collect()
}

fn style_from_text(text: Option<&Value>, offset_y: f64) -> CaptionStyle {
    let mut s = CaptionStyle { offset_y: Some(offset_y), ..CaptionStyle::default() };
    if let Some(t) = text {
        s.font_family = t.get("fontFamily").and_then(Value::as_str).map(str::to_owned);
        s.font_size = t.get("fontSize").and_then(Value::as_f64);
        s.font_weight = t.get("fontWeight").and_then(Value::as_f64);
        s.color = t.get("color").and_then(Value::as_str).map(str::to_owned);
    }
    s
}

/// Spreads a caption line's words evenly over its duration and maps them back to source time.
fn line_to_words(line: &CaptionLine, kept: &[TimeRange], out_starts: &[f64]) -> Vec<Word> {
    let parts: Vec<&str> = line.text.split_whitespace().collect();
    if parts.is_empty() {
        return Vec::new();
    }
    let step = (line.end - line.start) / parts.len() as f64;
    let to_src = |t: f64| {
        let i = out_starts.iter().rposition(|s| *s <= t + EPS).unwrap_or(0);
        kept[i].start + (t - out_starts[i]).clamp(0.0, kept[i].len())
    };
    parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (a, b) = (line.start + step * i as f64, line.start + step * (i + 1) as f64);
            Word { text: (*p).to_owned(), start: to_src(a + EPS), end: to_src(b - EPS).max(to_src(a + EPS)) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(a: f64, b: f64) -> TimeRange {
        TimeRange { start: a, end: b }
    }

    fn w(t: &str, a: f64, b: f64) -> Word {
        Word { text: t.into(), start: a, end: b }
    }

    #[test]
    fn ranges_merge_when_touching_and_reject_garbage() {
        let m = normalize_ranges(&[r(1.0, 2.0), r(2.0, 3.0), r(5.0, 6.0)]).unwrap();
        assert_eq!(m, vec![r(1.0, 3.0), r(5.0, 6.0)]);
        assert!(normalize_ranges(&[]).is_err());
        assert!(normalize_ranges(&[r(3.0, 2.0)]).is_err());
        assert!(normalize_ranges(&[r(5.0, 6.0), r(1.0, 2.0)]).is_err());
        assert!(normalize_ranges(&[r(f64::NAN, 2.0)]).is_err());
    }

    #[test]
    fn words_in_cut_regions_vanish_and_times_shift() {
        // keep [10,12) and [20,22): output 0..2 and 2..4.
        let ranges = [r(10.0, 12.0), r(20.0, 22.0)];
        let words = [w("a", 10.2, 10.6), w("cut", 15.0, 15.4), w("b", 20.5, 21.0), w("edge", 11.8, 12.4)];
        let mapped = map_words(&words, &ranges);
        let texts: Vec<_> = mapped.iter().map(|m| m.text.as_str()).collect();
        // "edge" has its midpoint (12.1) outside [10,12) so it goes with the cut.
        assert_eq!(texts, ["a", "b"]);
        assert!((mapped[0].start - 0.2).abs() < 1e-9);
        assert!((mapped[1].start - 2.5).abs() < 1e-9);
    }

    #[test]
    fn midpoint_rule_keeps_straddling_word_clamped() {
        let ranges = [r(10.0, 12.0)];
        let mapped = map_words(&[w("edge", 11.6, 12.3)], &ranges);
        assert_eq!(mapped.len(), 1);
        assert!((mapped[0].end - 2.0).abs() < 1e-9, "clamped to the range end");
    }

    #[test]
    fn lines_break_on_words_chars_gap_and_cuts() {
        let ranges = [r(0.0, 10.0), r(20.0, 30.0)];
        let words: Vec<Word> = (0..6).map(|i| w(&format!("w{i}"), f64::from(i) * 0.4, f64::from(i) * 0.4 + 0.3)).collect();
        let timed = map_words(&words, &ranges);
        let style = CaptionStyle { max_words_per_line: 4, ..CaptionStyle::default() };
        let lines = group_words(&timed, &style);
        assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["w0 w1 w2 w3", "w4 w5"]);
        // Gap break.
        let gap = map_words(&[w("a", 0.0, 0.3), w("b", 5.0, 5.3)], &ranges);
        assert_eq!(group_words(&gap, &CaptionStyle::default()).len(), 2);
        // Never across a cut even when close in output time.
        let cut = map_words(&[w("x", 9.5, 9.9), w("y", 20.0, 20.3)], &ranges);
        assert_eq!(group_words(&cut, &CaptionStyle::default()).len(), 2);
        // Uppercase + no overlap + min duration.
        let up = CaptionStyle { uppercase: true, ..CaptionStyle::default() };
        let l = group_words(&map_words(&[w("hi", 0.0, 0.01), w("yo", 0.05, 0.4)], &ranges), &CaptionStyle { max_words_per_line: 1, ..up });
        assert_eq!(l[0].text, "HI");
        assert!(l[0].end <= l[1].start + 1e-9);
    }

    #[test]
    fn crop_math_matches_concat_fill_and_inverts() {
        // 16:9 -> 9:16: fill scale = 1920/607.5... computed: fit=0.5625, fit_w=1080, fit_h=607.5.
        let (s, ox, oy) = crop_to_transform((1920.0, 1080.0), (1080.0, 1920.0), 0.5, 0.5, 1.0).unwrap();
        assert!((s - 1920.0 / 607.5).abs() < 1e-9);
        assert!(ox.abs() < 1e-12 && oy.abs() < 1e-12);
        // Subject at 75% of the source width: picture shifts left (negative).
        let (s2, ox2, _) = crop_to_transform((1920.0, 1080.0), (1080.0, 1920.0), 0.75, 0.5, 1.0).unwrap();
        assert!(ox2 < 0.0 && (s2 - s).abs() < 1e-12);
        let (cx, cy, z) = transform_to_crop((1920.0, 1080.0), (1080.0, 1920.0), s2, ox2, 0.0).unwrap();
        assert!((cx - 0.75).abs() < 1e-9 && (cy - 0.5).abs() < 1e-9 && (z - 1.0).abs() < 1e-9);
        // Subject at the very edge is clamped so the frame stays covered.
        let (_, ox3, _) = crop_to_transform((1920.0, 1080.0), (1080.0, 1920.0), 1.0, 0.5, 1.0).unwrap();
        let wfrac = s;
        assert!((ox3 + (wfrac - 1.0) / 2.0).abs() < 1e-9);
        // Max scale clamp.
        assert_eq!(crop_to_transform((1920.0, 1080.0), (1080.0, 1920.0), 0.5, 0.5, 100.0).unwrap().0, 8.0);
        assert!(crop_to_transform((0.0, 1.0), (1.0, 1.0), 0.5, 0.5, 1.0).is_none());
    }

    #[test]
    fn range_keys_interpolate_edges_and_collapse_constants() {
        let path = [
            CropKeyframe { time: 0.0, center_x: 0.2, center_y: 0.5, zoom: 1.0 },
            CropKeyframe { time: 4.0, center_x: 0.6, center_y: 0.5, zoom: 1.0 },
        ];
        // Range covering output 1..3: edges sampled at t=1 (0.3) and t=3 (0.5).
        let k = range_keys(&path, 1.0, 2.0);
        assert_eq!(k.len(), 2);
        assert!((k[0].center_x - 0.3).abs() < 1e-9 && (k[1].center_x - 0.5).abs() < 1e-9);
        // After the last key the path holds: constant -> single key.
        let k = range_keys(&path, 5.0, 2.0);
        assert_eq!(k.len(), 1);
        assert!((k[0].center_x - 0.6).abs() < 1e-9);
    }

    #[test]
    fn lowering_orders_commands_and_validates() {
        let mut plan = StudioClipPlan::new("clip", "/home/u/src.mp4", vec![r(10.0, 12.0), r(20.0, 22.0)]);
        plan.captions = Some(CaptionTrack { words: vec![w("hello", 10.1, 10.5), w("world", 10.6, 11.0)], style: CaptionStyle::default() });
        plan.overlays = vec![
            Overlay::Title { text: "Hook".into(), start: 0.0, duration: 1.5, style: None, offset_y: Some(-0.3) },
            Overlay::Media { file: "/home/u/broll.mp4".into(), start: 1.0, duration: 1.0, scale: 0.5, offset_x: 0.0, offset_y: 0.0, opacity: 1.0 },
        ];
        let cmds = to_concat_commands(&plan).unwrap();
        let tags: Vec<&str> = cmds
            .iter()
            .map(|c| match c {
                Command::ImportMedia { .. } => "import",
                Command::PlaceRange { .. } => "place",
                Command::Frame { .. } => "frame",
                Command::CropKeys { .. } => "crop",
                Command::AddCaptions { .. } => "captions",
                Command::AddTitle { .. } => "title",
                Command::AddMediaOverlay { .. } => "overlay",
            })
            .collect();
        assert_eq!(tags, ["import", "import", "place", "place", "frame", "frame", "captions", "title", "overlay"]);
        match &cmds[3] {
            Command::PlaceRange { at, source_start, .. } => assert_eq!((*at, *source_start), (2.0, 20.0)),
            _ => unreachable!(),
        }
        // Bad inputs.
        plan.source.clear();
        assert!(matches!(to_concat_commands(&plan), Err(ConcatError::InvalidPlan(_))));
        let mut p2 = StudioClipPlan::new("c", "/s.mp4", vec![r(0.0, 1.0)]);
        p2.overlays = vec![Overlay::Title { text: "x".into(), start: 0.0, duration: 0.0, style: None, offset_y: None }];
        assert!(to_concat_commands(&p2).is_err());
    }

    #[test]
    fn plan_json_roundtrips() {
        let mut plan = StudioClipPlan::new("c", "/s.mp4", vec![r(1.0, 2.0)]);
        plan.crop_keyframes = vec![CropKeyframe { time: 0.0, center_x: 0.4, center_y: 0.5, zoom: 1.2 }];
        let v = serde_json::to_value(&plan).unwrap();
        let back: StudioClipPlan = serde_json::from_value(v).unwrap();
        assert_eq!(back, plan);
        // Minimal JSON from an agent works through defaults.
        let min: StudioClipPlan = serde_json::from_value(json!({
            "name": "c", "source": "/s.mp4", "kept_ranges": [{"start": 0.0, "end": 3.0}],
            "captions": {"words": [{"text": "hi", "start": 0.0, "end": 0.5}]}
        }))
        .unwrap();
        assert_eq!(min.video, VideoSettings::VERTICAL_1080);
        assert_eq!(min.captions.unwrap().style.max_words_per_line, 4);
    }
}
