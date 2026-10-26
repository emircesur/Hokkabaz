//! Long-form outputs from a transcript (P9.33, P9.35, P9.30): YouTube chapters, written
//! pieces (blog, newsletter, thread, summary), hook review of a clip opening, and caption
//! translation that keeps the original timing.

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::llm::{Llm, complete_json};
use hk_stt::Transcript;

/// One chapter marker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    pub start: f64,
    pub title: String,
}

/// `[mm:ss] text` lines from transcript segments (words grouped per ~20 s when unsegmented).
pub fn timed_text(tr: &Transcript, max_chars: usize) -> String {
    let mut out = String::new();
    let mut push = |start: f64, text: &str| {
        if out.len() < max_chars {
            out.push_str(&format!("[{}] {}\n", clock(start), text.trim()));
        }
    };
    if !tr.segments.is_empty() {
        for s in &tr.segments {
            push(s.start, &s.text);
        }
    } else {
        let mut start = None;
        let mut buf = Vec::new();
        for w in &tr.words {
            let s = *start.get_or_insert(w.start);
            buf.push(w.text.as_str());
            if w.end - s >= 20.0 {
                push(s, &buf.join(" "));
                buf.clear();
                start = None;
            }
        }
        if let Some(s) = start {
            push(s, &buf.join(" "));
        }
    }
    out
}

/// `m:ss` / `h:mm:ss` as YouTube expects.
pub fn clock(t: f64) -> String {
    let t = t.max(0.0) as u64;
    let (h, m, s) = (t / 3600, (t % 3600) / 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Apply YouTube's chapter rules: first at 0:00, ≥ 10 s apart, ≥ 3 chapters (else empty).
pub fn normalize_chapters(mut ch: Vec<Chapter>, duration: f64) -> Vec<Chapter> {
    ch.retain(|c| {
        c.start >= 0.0 && (duration <= 0.0 || c.start < duration) && !c.title.trim().is_empty()
    });
    ch.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Chapter> = Vec::new();
    for c in ch {
        if out.last().is_none_or(|l| c.start - l.start >= 10.0) {
            out.push(c);
        }
    }
    if let Some(f) = out.first_mut() {
        f.start = 0.0;
    }
    if out.len() < 3 { Vec::new() } else { out }
}

/// Description block: one `m:ss Title` per line.
pub fn youtube_timestamps(ch: &[Chapter]) -> String {
    ch.iter()
        .map(|c| format!("{} {}", clock(c.start), c.title.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

const CHAPTERS_SYSTEM: &str = "You split videos into chapters. Reply with JSON only: \
{\"chapters\":[{\"start\":<seconds>,\"title\":\"<= 60 chars\"}]}. Use topic changes, \
5-12 chapters for long videos, first chapter at 0, titles in the transcript's language.";

pub async fn chapters(llm: &dyn Llm, tr: &Transcript) -> Result<Vec<Chapter>> {
    #[derive(Deserialize)]
    struct Out {
        chapters: Vec<Chapter>,
    }
    let user = format!("Transcript with timestamps:\n{}", timed_text(tr, 60_000));
    let out: Out = complete_json(llm, CHAPTERS_SYSTEM, &user).await?;
    Ok(normalize_chapters(out.chapters, tr.duration))
}

/// Kind of written piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LongformKind {
    Blog,
    Newsletter,
    Thread,
    Summary,
}

impl LongformKind {
    fn brief(self) -> &'static str {
        match self {
            Self::Blog => "a blog post in Markdown: title, short intro, H2 sections, conclusion",
            Self::Newsletter => {
                "a newsletter issue in Markdown: subject line, greeting, 3-5 short sections, sign-off"
            }
            Self::Thread => {
                "a social thread: 5-10 numbered posts, each <= 280 characters, first post is the hook"
            }
            Self::Summary => "a summary: 1-paragraph TL;DR then 5-8 bullet key points",
        }
    }
}

pub async fn write_longform(
    llm: &dyn Llm,
    title: &str,
    text: &str,
    kind: LongformKind,
    language: Option<&str>,
) -> Result<String> {
    let lang = language.map_or(String::new(), |l| format!(" Write in {l}."));
    let system = format!(
        "You turn video transcripts into {}. Keep facts from the transcript only; no invented quotes.{lang} Reply with the text only.",
        kind.brief()
    );
    let user = format!(
        "Video title: {title}\n\nTranscript:\n{}",
        truncate_chars(text, 60_000)
    );
    Ok(llm.complete(&system, &user).await?.trim().to_string())
}

/// Review of a clip opening.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookReport {
    /// 0..100.
    pub score: u8,
    pub issues: Vec<String>,
    /// Rewritten opening lines (spoken or on-screen text).
    pub rewrites: Vec<String>,
    /// Suggested title card text for the first seconds.
    pub title_card: Option<String>,
    /// Index of a word in the clip that would make a stronger start, if any.
    pub better_start_word: Option<usize>,
}

const HOOK_SYSTEM: &str = "You review the first seconds of short-form videos. Score how well \
they stop the scroll (curiosity, stakes, clarity, pace). Reply JSON only: {\"score\":0-100,\
\"issues\":[..],\"rewrites\":[3 options],\"title_card\":\"<= 40 chars\",\"better_start_word\":<index or null>}.";

/// `words` are the clip's words in order; the first ~3 s are what viewers judge.
pub async fn hook_doctor(
    llm: &dyn Llm,
    clip_title: &str,
    words: &[(f64, &str)],
) -> Result<HookReport> {
    let opening: Vec<&str> = words
        .iter()
        .take_while(|(t, _)| *t - words.first().map_or(0.0, |w| w.0) <= 3.5)
        .map(|(_, w)| *w)
        .collect();
    let indexed: Vec<String> = words
        .iter()
        .take(200)
        .enumerate()
        .map(|(i, (_, w))| format!("{i}:{w}"))
        .collect();
    let user = format!(
        "Clip title: {clip_title}\nFirst 3 seconds: \"{}\"\nWords (index:word): {}",
        opening.join(" "),
        indexed.join(" ")
    );
    let mut r: HookReport = complete_json(llm, HOOK_SYSTEM, &user).await?;
    r.score = r.score.min(100);
    r.better_start_word = r.better_start_word.filter(|&i| i < words.len());
    Ok(r)
}

/// A caption line with timing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimedLine {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

const TRANSLATE_SYSTEM: &str = "You translate subtitle lines. Keep the same number of lines and \
order, natural spoken style, similar length. Reply JSON only: {\"lines\":[\"...\"]}.";

/// Translate lines in batches of 40; timing is kept, so captions stay in sync.
pub async fn translate_lines(
    llm: &dyn Llm,
    lines: &[TimedLine],
    target: &str,
) -> Result<Vec<TimedLine>> {
    #[derive(Deserialize)]
    struct Out {
        lines: Vec<String>,
    }
    let mut out = Vec::with_capacity(lines.len());
    for chunk in lines.chunks(40) {
        let numbered: Vec<String> = chunk.iter().map(|l| l.text.clone()).collect();
        let user = format!(
            "Target language: {target}\nLines ({}):\n{}",
            chunk.len(),
            serde_json::to_string(&numbered).unwrap_or_default()
        );
        let o: Out = complete_json(llm, TRANSLATE_SYSTEM, &user).await?;
        for (i, l) in chunk.iter().enumerate() {
            // A short reply keeps the original line rather than shifting the rest.
            let text = o.lines.get(i).cloned().unwrap_or_else(|| l.text.clone());
            out.push(TimedLine { text, ..l.clone() });
        }
    }
    Ok(out)
}

/// Lines from transcript segments (or ~6-word groups when unsegmented).
pub fn transcript_lines(tr: &Transcript) -> Vec<TimedLine> {
    if !tr.segments.is_empty() {
        return tr
            .segments
            .iter()
            .map(|s| TimedLine {
                start: s.start,
                end: s.end,
                text: s.text.trim().to_string(),
            })
            .collect();
    }
    tr.words
        .chunks(6)
        .filter_map(|c| {
            Some(TimedLine {
                start: c.first()?.start,
                end: c.last()?.end,
                text: c
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            })
        })
        .collect()
}

/// SRT text for timed lines.
pub fn to_srt(lines: &[TimedLine]) -> String {
    fn ts(t: f64) -> String {
        let ms = (t.max(0.0) * 1000.0).round() as u64;
        format!(
            "{:02}:{:02}:{:02},{:03}",
            ms / 3_600_000,
            (ms / 60_000) % 60,
            (ms / 1000) % 60,
            ms % 1000
        )
    }
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{}\n{} --> {}\n{}\n", i + 1, ts(l.start), ts(l.end), l.text))
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ScriptedLlm;

    #[test]
    fn chapter_rules() {
        let c = |s: f64, t: &str| Chapter {
            start: s,
            title: t.into(),
        };
        let n = normalize_chapters(
            vec![
                c(65.0, "B"),
                c(3.0, "A"),
                c(70.0, "too close"),
                c(200.0, "C"),
            ],
            300.0,
        );
        assert_eq!(youtube_timestamps(&n), "0:00 A\n1:05 B\n3:20 C");
        assert!(normalize_chapters(vec![c(0.0, "A"), c(30.0, "B")], 60.0).is_empty());
        assert_eq!(clock(3725.0), "1:02:05");
    }

    #[test]
    fn srt_format() {
        let l = [TimedLine {
            start: 1.5,
            end: 3.25,
            text: "Merhaba".into(),
        }];
        assert_eq!(to_srt(&l), "1\n00:00:01,500 --> 00:00:03,250\nMerhaba\n");
    }

    #[tokio::test]
    async fn translate_keeps_timing_and_count() {
        let llm = ScriptedLlm::new([r#"{"lines":["Selam"]}"#]);
        let lines = vec![
            TimedLine {
                start: 0.0,
                end: 1.0,
                text: "Hi".into(),
            },
            TimedLine {
                start: 1.0,
                end: 2.0,
                text: "there".into(),
            },
        ];
        let out = translate_lines(&llm, &lines, "tr").await.unwrap();
        assert_eq!(out[0].text, "Selam");
        assert_eq!(out[1].text, "there");
        assert_eq!(out[1].start, 1.0);
    }
}
