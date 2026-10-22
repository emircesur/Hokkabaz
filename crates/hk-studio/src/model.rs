use std::collections::BTreeSet;
use std::path::PathBuf;

use hk_media::{CropKey, CropPath, Interp, MediaInfo};
use hk_stt::{Transcript, Word};
use serde::{Deserialize, Serialize};

use crate::{Result, invalid};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

impl Span {
    pub fn duration(self) -> f64 {
        self.end - self.start
    }
    pub fn validate(self, duration: f64) -> Result<()> {
        if self.start.is_finite()
            && self.end.is_finite()
            && self.start >= 0.0
            && self.end > self.start
            && self.end <= duration + 0.001
        {
            Ok(())
        } else {
            Err(invalid("invalid source time range"))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub path: PathBuf,
    pub info: MediaInfo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptionStyle {
    pub enabled: bool,
    pub font: String,
    pub font_size: u32,
    pub color: String,
    pub highlight: String,
    pub outline: u32,
    pub margin_bottom: u32,
    pub words_per_line: usize,
    pub uppercase: bool,
    pub animated: bool,
}

impl Default for CaptionStyle {
    fn default() -> Self {
        Self {
            enabled: true,
            font: "DejaVu Sans".into(),
            font_size: 64,
            color: "#FFFFFF".into(),
            highlight: "#FFD54F".into(),
            outline: 4,
            margin_bottom: 200,
            words_per_line: 4,
            uppercase: false,
            animated: true,
        }
    }
}

impl CaptionStyle {
    pub fn validate(&self) -> Result<()> {
        for color in [&self.color, &self.highlight] {
            if color.len() != 7
                || !color.starts_with('#')
                || !color[1..].bytes().all(|c| c.is_ascii_hexdigit())
            {
                return Err(invalid("caption colors must use #RRGGBB"));
            }
        }
        if self.font.is_empty()
            || self.font.len() > 100
            || self.font.contains([',', '\n', '\r'])
            || !(12..=200).contains(&self.font_size)
            || !(1..=12).contains(&self.words_per_line)
            || self.outline > 20
            || self.margin_bottom > 1800
        {
            return Err(invalid("invalid caption style"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aspect {
    #[default]
    Vertical,
    Square,
    Landscape,
}

impl Aspect {
    pub fn size(self) -> (u32, u32) {
        match self {
            Self::Vertical => (1080, 1920),
            Self::Square => (1080, 1080),
            Self::Landscape => (1920, 1080),
        }
    }
}

/// Source-time subject positions, normalized to 0..1. Scene cuts reset smoothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubjectPoint {
    pub time: f64,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub scene_cut: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Broll {
    pub path: PathBuf,
    /// Edited output timeline, in seconds.
    pub span: Span,
    pub attribution: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Edit {
    pub deleted_words: BTreeSet<usize>,
    pub restored_words: BTreeSet<usize>,
    pub remove_fillers: bool,
    pub remove_silences: bool,
    pub silence_threshold: f64,
    pub padding: f64,
    pub aspect: Aspect,
    pub captions: CaptionStyle,
    pub subject: Vec<SubjectPoint>,
    pub broll: Vec<Broll>,
    pub template: String,
    pub music: Option<PathBuf>,
}

impl Default for Edit {
    fn default() -> Self {
        Self {
            deleted_words: BTreeSet::new(),
            restored_words: BTreeSet::new(),
            remove_fillers: false,
            remove_silences: false,
            silence_threshold: 0.6,
            padding: 0.04,
            aspect: Aspect::default(),
            captions: CaptionStyle::default(),
            subject: Vec::new(),
            broll: Vec::new(),
            template: "karaoke".into(),
            music: None,
        }
    }
}

impl Edit {
    pub fn validate(&self, duration: f64, words: usize) -> Result<()> {
        self.captions.validate()?;
        if !self.silence_threshold.is_finite()
            || !(0.1..=10.0).contains(&self.silence_threshold)
            || !self.padding.is_finite()
            || !(0.0..=0.3).contains(&self.padding)
            || self
                .deleted_words
                .iter()
                .chain(&self.restored_words)
                .any(|i| *i >= words)
        {
            return Err(invalid("invalid transcript edit"));
        }
        let mut previous = -1.0;
        for point in &self.subject {
            if !point.time.is_finite()
                || point.time < 0.0
                || point.time > duration
                || point.time <= previous
                || !point.x.is_finite()
                || !point.y.is_finite()
                || !(0.0..=1.0).contains(&point.x)
                || !(0.0..=1.0).contains(&point.y)
            {
                return Err(invalid(
                    "subject points must be ordered and inside the source",
                ));
            }
            previous = point.time;
        }
        for overlay in &self.broll {
            overlay.span.validate(duration)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Criterion {
    pub name: String,
    pub score: u32,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub span: Span,
    pub title: String,
    pub hook: String,
    pub reason: String,
    pub score: u32,
    pub criteria: Vec<Criterion>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Social {
    #[serde(default)]
    pub clip_id: Option<String>,
    pub platform: String,
    pub title: String,
    pub description: String,
    pub hashtags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub source: Source,
    pub transcript: Option<Transcript>,
    pub edit: Edit,
    pub candidates: Vec<Candidate>,
    pub social: Vec<Social>,
    #[serde(default)]
    pub undo: Vec<Edit>,
    #[serde(default)]
    pub redo: Vec<Edit>,
}

impl Project {
    pub fn duration(&self) -> f64 {
        self.source.info.duration.unwrap_or(0.0)
    }

    pub fn set_edit(&mut self, edit: Edit) -> Result<()> {
        edit.validate(
            self.duration(),
            self.transcript.as_ref().map_or(0, |t| t.words.len()),
        )?;
        self.undo.push(std::mem::replace(&mut self.edit, edit));
        if self.undo.len() > 50 {
            self.undo.remove(0);
        }
        self.redo.clear();
        Ok(())
    }

    /// Subtract explicit word cuts and transcript gaps, preserving unedited media edges.
    pub fn kept_ranges(&self, clip: Option<Span>) -> Result<Vec<Span>> {
        let window = clip.unwrap_or(Span {
            start: 0.0,
            end: self.duration(),
        });
        window.validate(self.duration())?;
        let mut removed = Vec::new();
        if let Some(t) = &self.transcript {
            for (i, w) in t.words.iter().enumerate() {
                if self.edit.deleted_words.contains(&i)
                    || (self.edit.remove_fillers
                        && w.flags.filler
                        && !self.edit.restored_words.contains(&i))
                {
                    // Padding must never preserve a deleted word's spoken interval.
                    removed.push(Span {
                        start: w.start,
                        end: w.end,
                    });
                }
            }
            if self.edit.remove_silences {
                for gap in t.silence_gaps(self.edit.silence_threshold) {
                    let start = gap.start + self.edit.padding;
                    let end = gap.end - self.edit.padding;
                    if end > start {
                        removed.push(Span { start, end });
                    }
                }
            }
        }
        removed.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut cursor = window.start;
        let mut kept = Vec::new();
        for cut in removed {
            if cut.start > cursor {
                kept.push(Span {
                    start: cursor,
                    end: cut.start.min(window.end),
                });
            }
            cursor = cursor.max(cut.end).min(window.end);
            if cursor >= window.end {
                break;
            }
        }
        if cursor < window.end {
            kept.push(Span {
                start: cursor,
                end: window.end,
            });
        }
        kept.retain(|s| s.duration() >= 0.001);
        Ok(kept)
    }

    pub fn output_words(&self, ranges: &[Span]) -> Vec<Word> {
        let Some(transcript) = &self.transcript else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut offset = 0.0;
        for span in ranges {
            for (i, word) in transcript.words.iter().enumerate() {
                if word.start >= span.end
                    || word.end <= span.start
                    || self.edit.deleted_words.contains(&i)
                    || (self.edit.remove_fillers
                        && word.flags.filler
                        && !self.edit.restored_words.contains(&i))
                {
                    continue;
                }
                let mut mapped = word.clone();
                mapped.start = offset + (word.start.max(span.start) - span.start);
                mapped.end = offset + (word.end.min(span.end) - span.start);
                out.push(mapped);
            }
            offset += span.duration();
        }
        out
    }

    pub fn crop_path(&self, ranges: &[Span]) -> Option<CropPath> {
        let (sw, sh) = self.source.info.display_size()?;
        let (ow, oh) = self.edit.aspect.size();
        let ratio = f64::from(ow) / f64::from(oh);
        let width = f64::from(sw).min(f64::from(sh) * ratio);
        let height = width / ratio;
        let mut keys = Vec::new();
        let mut offset = 0.0;
        let mut smoothed = (0.5_f64, 0.5_f64);
        for span in ranges {
            let initial = self
                .edit
                .subject
                .iter()
                .rev()
                .find(|p| p.time <= span.start);
            if let Some(p) = initial {
                smoothed = (p.x, p.y);
            }
            let points = std::iter::once((span.start, smoothed.0, smoothed.1, true)).chain(
                self.edit
                    .subject
                    .iter()
                    .filter(|p| p.time > span.start && p.time < span.end)
                    .map(|p| (p.time, p.x, p.y, p.scene_cut)),
            );
            for (time, x, y, reset) in points {
                smoothed = if reset {
                    (x, y)
                } else {
                    (smoothed.0 * 0.55 + x * 0.45, smoothed.1 * 0.55 + y * 0.45)
                };
                keys.push(CropKey {
                    t: offset + time - span.start,
                    x: (smoothed.0 * f64::from(sw) - width / 2.0).clamp(0.0, f64::from(sw) - width),
                    y: (smoothed.1 * f64::from(sh) - height / 2.0)
                        .clamp(0.0, f64::from(sh) - height),
                    w: width,
                    h: height,
                });
            }
            offset += span.duration();
        }
        Some(CropPath {
            keys,
            interp: Interp::Smooth,
        })
    }

    /// Retiming overlays from the full edited timeline into a selected short clip.
    pub fn mapped_broll(&self, clip: Option<Span>) -> Result<Vec<(Broll, f64)>> {
        let offset = if let Some(clip) = clip {
            self.kept_ranges(None)?
                .iter()
                .map(|range| (clip.start.min(range.end) - range.start).clamp(0.0, range.duration()))
                .sum()
        } else {
            0.0
        };
        let duration = self
            .kept_ranges(clip)?
            .iter()
            .map(|range| range.duration())
            .sum::<f64>();
        Ok(self
            .edit
            .broll
            .iter()
            .filter_map(|overlay| {
                let start = overlay.span.start.max(offset);
                let end = overlay.span.end.min(offset + duration);
                (end > start).then(|| {
                    (
                        Broll {
                            span: Span {
                                start: start - offset,
                                end: end - offset,
                            },
                            ..overlay.clone()
                        },
                        start - overlay.span.start,
                    )
                })
            })
            .collect())
    }
}

pub fn templates() -> Vec<(&'static str, CaptionStyle)> {
    let base = CaptionStyle::default();
    vec![
        ("karaoke", base.clone()),
        (
            "bold_pop",
            CaptionStyle {
                font_size: 78,
                uppercase: true,
                highlight: "#B7FF4A".into(),
                words_per_line: 3,
                ..base.clone()
            },
        ),
        (
            "podcast",
            CaptionStyle {
                font_size: 54,
                highlight: "#7DD3FC".into(),
                animated: false,
                words_per_line: 6,
                ..base.clone()
            },
        ),
        (
            "minimal",
            CaptionStyle {
                font_size: 48,
                highlight: "#FFFFFF".into(),
                outline: 2,
                animated: false,
                ..base.clone()
            },
        ),
        (
            "neon",
            CaptionStyle {
                highlight: "#F472FF".into(),
                outline: 6,
                uppercase: true,
                ..base.clone()
            },
        ),
        (
            "cinematic",
            CaptionStyle {
                font_size: 48,
                highlight: "#FDE68A".into(),
                margin_bottom: 140,
                animated: false,
                ..base.clone()
            },
        ),
        (
            "news",
            CaptionStyle {
                highlight: "#60A5FA".into(),
                words_per_line: 7,
                animated: false,
                ..base.clone()
            },
        ),
        (
            "lecture",
            CaptionStyle {
                font_size: 50,
                words_per_line: 8,
                highlight: "#D9F99D".into(),
                ..base.clone()
            },
        ),
        (
            "gaming",
            CaptionStyle {
                font_size: 80,
                words_per_line: 2,
                highlight: "#C084FC".into(),
                outline: 6,
                uppercase: true,
                ..base.clone()
            },
        ),
        (
            "clean_white",
            CaptionStyle {
                highlight: "#FFFFFF".into(),
                outline: 3,
                words_per_line: 5,
                ..base.clone()
            },
        ),
        (
            "story",
            CaptionStyle {
                highlight: "#FDA4AF".into(),
                margin_bottom: 300,
                ..base.clone()
            },
        ),
        (
            "kinetic",
            CaptionStyle {
                font_size: 90,
                words_per_line: 1,
                highlight: "#FB923C".into(),
                uppercase: true,
                ..base
            },
        ),
    ]
}
