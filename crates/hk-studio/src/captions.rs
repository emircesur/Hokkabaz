//! Caption words on the output timeline, styles, and ASS / SRT / VTT writers.
//!
//! ASS is what ffmpeg burns in (`hk_media::RenderSpec::subtitles`). Karaoke
//! mode emits one event per word so exactly the active word is highlighted.

use std::fmt::Write as _;

use hk_stt::Transcript;
use serde::{Deserialize, Serialize};

use crate::edl::OutputMap;
use crate::project::Clip;

/// A caption word with times on the OUTPUT timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionMode {
    /// Whole line shown at once.
    Block,
    /// Whole line shown; the spoken word is highlighted.
    Karaoke,
    /// One word at a time.
    WordByWord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptionAnim {
    None,
    Pop,
    Fade,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VPos {
    Bottom,
    Center,
    Top,
}

/// Caption look. `size` is in pixels for a 1080-high frame and scales with the output height.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptionStyle {
    pub font: String,
    pub size: u32,
    /// `#RRGGBB`.
    pub primary: String,
    /// Active-word colour (karaoke / word-by-word).
    pub highlight: String,
    pub outline_color: String,
    pub outline: f32,
    pub shadow: f32,
    pub bold: bool,
    pub uppercase: bool,
    pub mode: CaptionMode,
    pub anim: CaptionAnim,
    pub pos: VPos,
    /// Distance from the edge, in 1080-high pixels.
    pub margin_v: u32,
    pub max_words: usize,
    pub max_chars: usize,
    /// A pause longer than this starts a new line (seconds).
    pub max_gap: f64,
}

impl Default for CaptionStyle {
    fn default() -> Self {
        Self::classic()
    }
}

impl CaptionStyle {
    pub fn classic() -> Self {
        Self {
            font: "Arial".into(),
            size: 56,
            primary: "#FFFFFF".into(),
            highlight: "#FFFFFF".into(),
            outline_color: "#000000".into(),
            outline: 3.0,
            shadow: 1.0,
            bold: true,
            uppercase: false,
            mode: CaptionMode::Block,
            anim: CaptionAnim::Fade,
            pos: VPos::Bottom,
            margin_v: 160,
            max_words: 7,
            max_chars: 38,
            max_gap: 0.8,
        }
    }

    /// Yellow active word on white text, centred low: the short-form look.
    pub fn karaoke() -> Self {
        Self {
            size: 72,
            highlight: "#FFE600".into(),
            outline: 4.0,
            mode: CaptionMode::Karaoke,
            anim: CaptionAnim::Pop,
            uppercase: true,
            max_words: 4,
            max_chars: 22,
            pos: VPos::Center,
            margin_v: 0,
            ..Self::classic()
        }
    }

    /// One big word at a time.
    pub fn pop() -> Self {
        Self {
            size: 110,
            highlight: "#39FF14".into(),
            primary: "#FFFFFF".into(),
            outline: 6.0,
            mode: CaptionMode::WordByWord,
            anim: CaptionAnim::Pop,
            uppercase: true,
            pos: VPos::Center,
            margin_v: 0,
            max_words: 1,
            max_chars: 20,
            ..Self::classic()
        }
    }

    pub fn minimal() -> Self {
        Self { size: 44, outline: 2.0, shadow: 0.0, bold: false, anim: CaptionAnim::None, margin_v: 90, ..Self::classic() }
    }

    pub fn preset(name: &str) -> Option<Self> {
        Some(match name {
            "classic" => Self::classic(),
            "karaoke" => Self::karaoke(),
            "pop" => Self::pop(),
            "minimal" => Self::minimal(),
            _ => return None,
        })
    }
}

/// Words of `clip` that survive its cuts, retimed onto the output timeline.
pub fn words_for_clip(tr: &Transcript, clip: &Clip, map: &OutputMap) -> Vec<CapWord> {
    let mut out = Vec::new();
    for (i, w) in tr.words.iter().enumerate() {
        if w.end <= clip.start || w.start >= clip.end {
            continue;
        }
        if clip.deleted_words.contains(&i) || (clip.remove_fillers && w.flags.filler) {
            continue;
        }
        let (Some(s), Some(e)) = (map.to_output(w.start.max(clip.start)), map.to_output(w.end.min(clip.end))) else {
            continue;
        };
        if e > s {
            out.push(CapWord { text: w.text.trim().to_string(), start: s, end: e });
        }
    }
    out
}

/// Group words into display lines.
pub fn group_lines(words: &[CapWord], style: &CaptionStyle) -> Vec<Vec<CapWord>> {
    let mut lines: Vec<Vec<CapWord>> = Vec::new();
    let mut cur: Vec<CapWord> = Vec::new();
    let mut chars = 0usize;
    for w in words {
        let starts_new = cur.last().is_some_and(|p| {
            w.start - p.end > style.max_gap
                || cur.len() >= style.max_words.max(1)
                || chars + 1 + w.text.chars().count() > style.max_chars.max(1)
                || p.text.ends_with(['.', '!', '?'])
        });
        if starts_new {
            lines.push(std::mem::take(&mut cur));
            chars = 0;
        }
        chars += w.text.chars().count() + usize::from(!cur.is_empty());
        cur.push(w.clone());
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// `#RRGGBB` to ASS `&H00BBGGRR`.
pub fn ass_color(hex: &str) -> String {
    let h = hex.trim_start_matches('#');
    let (r, g, b) = if h.len() == 6 {
        (&h[0..2], &h[2..4], &h[4..6])
    } else {
        ("FF", "FF", "FF")
    };
    format!("&H00{}{}{}", b.to_uppercase(), g.to_uppercase(), r.to_uppercase())
}

fn ass_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360_000, (cs / 6000) % 60, (cs / 100) % 60, cs % 100)
}

fn escape(text: &str, upper: bool) -> String {
    let t: String = text.chars().map(|c| if c == '{' { '(' } else if c == '}' { ')' } else if c == '\\' || c == '\n' { ' ' } else { c }).collect();
    if upper { t.to_uppercase() } else { t }
}

/// Build an ASS script for `words` (output timeline) on a `play` sized canvas.
pub fn to_ass(words: &[CapWord], style: &CaptionStyle, play: (u32, u32)) -> String {
    let (pw, ph) = (play.0.max(2), play.1.max(2));
    let scale = f64::from(ph) / 1080.0;
    let size = (f64::from(style.size) * scale).round().max(8.0) as u32;
    let margin_v = (f64::from(style.margin_v) * scale).round() as u32;
    let align = match style.pos {
        VPos::Bottom => 2,
        VPos::Center => 5,
        VPos::Top => 8,
    };
    let mut s = String::new();
    let _ = write!(
        s,
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {pw}\nPlayResY: {ph}\nWrapStyle: 0\nScaledBorderAndShadow: yes\n\n\
         [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Default,{font},{size},{primary},{primary},{outline_c},&H80000000,{bold},0,0,0,100,100,0,0,1,{outline:.1},{shadow:.1},{align},{ml},{ml},{margin_v},1\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        font = style.font.replace(',', " "),
        primary = ass_color(&style.primary),
        outline_c = ass_color(&style.outline_color),
        bold = if style.bold { -1 } else { 0 },
        outline = f64::from(style.outline) * scale,
        shadow = f64::from(style.shadow) * scale,
        ml = (f64::from(pw) * 0.06).round() as u32,
    );
    let hi = ass_color(&style.highlight);
    let base = ass_color(&style.primary);
    let anim = match style.anim {
        CaptionAnim::None => "",
        CaptionAnim::Fade => "\\fad(80,80)",
        CaptionAnim::Pop => "\\fscx85\\fscy85\\t(0,90,\\fscx100\\fscy100)",
    };
    for line in group_lines(words, style) {
        let Some(first) = line.first() else { continue };
        let line_end = line.last().map_or(first.end, |w| w.end);
        match style.mode {
            CaptionMode::Block => {
                let text = line.iter().map(|w| escape(&w.text, style.uppercase)).collect::<Vec<_>>().join(" ");
                let _ = writeln!(s, "Dialogue: 0,{},{},Default,,0,0,0,,{{{anim}}}{text}", ass_time(first.start), ass_time(line_end));
            }
            CaptionMode::Karaoke => {
                for (k, w) in line.iter().enumerate() {
                    let end = line.get(k + 1).map_or(line_end, |n| n.start.max(w.end.min(n.start)));
                    let end = if end <= w.start { w.end } else { end };
                    let text = line
                        .iter()
                        .enumerate()
                        .map(|(j, x)| {
                            let t = escape(&x.text, style.uppercase);
                            if j == k { format!("{{\\c{hi}}}{t}{{\\c{base}}}") } else { t }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    let _ = writeln!(s, "Dialogue: 0,{},{},Default,,0,0,0,,{{{anim}}}{text}", ass_time(w.start), ass_time(end));
                }
            }
            CaptionMode::WordByWord => {
                for w in &line {
                    let t = escape(&w.text, style.uppercase);
                    let _ = writeln!(s, "Dialogue: 0,{},{},Default,,0,0,0,,{{\\c{hi}{anim}}}{t}", ass_time(w.start), ass_time(w.end));
                }
            }
        }
    }
    s
}

fn srt_time(t: f64, sep: char) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02}{sep}{:03}", ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, ms % 1000)
}

/// SRT, one cue per display line.
pub fn to_srt(words: &[CapWord], style: &CaptionStyle) -> String {
    let mut s = String::new();
    for (n, line) in group_lines(words, style).iter().enumerate() {
        let (Some(a), Some(b)) = (line.first(), line.last()) else { continue };
        let text = line.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        let _ = write!(s, "{}\n{} --> {}\n{text}\n\n", n + 1, srt_time(a.start, ','), srt_time(b.end, ','));
    }
    s
}

/// WebVTT, one cue per display line.
pub fn to_vtt(words: &[CapWord], style: &CaptionStyle) -> String {
    let mut s = String::from("WEBVTT\n\n");
    for line in group_lines(words, style) {
        let (Some(a), Some(b)) = (line.first(), line.last()) else { continue };
        let text = line.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        let _ = write!(s, "{} --> {}\n{text}\n\n", srt_time(a.start, '.'), srt_time(b.end, '.'));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(t: &str, s: f64, e: f64) -> CapWord {
        CapWord { text: t.into(), start: s, end: e }
    }

    #[test]
    fn colors_convert_to_bgr() {
        assert_eq!(ass_color("#FFE600"), "&H0000E6FF");
        assert_eq!(ass_color("#102030"), "&H00302010");
        assert_eq!(ass_color("bad"), "&H00FFFFFF");
    }

    #[test]
    fn times_format() {
        assert_eq!(ass_time(3661.5), "1:01:01.50");
        assert_eq!(srt_time(1.234, ','), "00:00:01,234");
        assert_eq!(srt_time(61.0, '.'), "00:01:01.000");
    }

    #[test]
    fn lines_split_on_gap_words_and_punctuation() {
        let words = vec![w("a", 0.0, 0.2), w("b", 0.3, 0.5), w("end.", 0.6, 0.9), w("next", 1.0, 1.2), w("late", 3.0, 3.2)];
        let st = CaptionStyle { max_words: 10, ..CaptionStyle::classic() };
        let lines = group_lines(&words, &st);
        assert_eq!(lines.iter().map(Vec::len).collect::<Vec<_>>(), vec![3, 1, 1]);
        let st = CaptionStyle { max_words: 2, ..CaptionStyle::classic() };
        assert_eq!(group_lines(&words[..2], &st).len(), 1);
        assert_eq!(group_lines(&words[..3], &st).len(), 2);
    }

    #[test]
    fn karaoke_emits_event_per_word_with_highlight() {
        let words = vec![w("hello", 0.0, 0.4), w("world", 0.5, 1.0)];
        let ass = to_ass(&words, &CaptionStyle::karaoke(), (1080, 1920));
        assert!(ass.contains("PlayResX: 1080"));
        assert_eq!(ass.matches("Dialogue:").count(), 2);
        assert!(ass.contains("{\\c&H0000E6FF}HELLO{\\c&H00FFFFFF} WORLD"));
        assert!(ass.contains("HELLO {\\c&H0000E6FF}WORLD"));
        // Fontsize scaled for 1920 high canvas: 72 * 1920/1080.
        assert!(ass.contains(",128,"));
    }

    #[test]
    fn block_and_word_modes() {
        let words = vec![w("one", 0.0, 0.3), w("two", 0.4, 0.8)];
        let ass = to_ass(&words, &CaptionStyle::classic(), (1920, 1080));
        assert_eq!(ass.matches("Dialogue:").count(), 1);
        assert!(ass.contains("one two"));
        let ass = to_ass(&words, &CaptionStyle::pop(), (1080, 1920));
        assert_eq!(ass.matches("Dialogue:").count(), 2);
    }

    #[test]
    fn escapes_braces_and_backslashes() {
        let words = vec![w("a{b}\\c", 0.0, 0.5)];
        let ass = to_ass(&words, &CaptionStyle::classic(), (100, 100));
        assert!(ass.contains("a(b) c"));
    }

    #[test]
    fn srt_and_vtt() {
        let words = vec![w("hi", 0.0, 0.5), w("there", 0.6, 1.0)];
        let srt = to_srt(&words, &CaptionStyle::classic());
        assert!(srt.starts_with("1\n00:00:00,000 --> 00:00:01,000\nhi there"));
        assert!(to_vtt(&words, &CaptionStyle::classic()).starts_with("WEBVTT"));
    }

    #[test]
    fn presets_resolve() {
        for n in ["classic", "karaoke", "pop", "minimal"] {
            assert!(CaptionStyle::preset(n).is_some());
        }
        assert!(CaptionStyle::preset("x").is_none());
    }
}
