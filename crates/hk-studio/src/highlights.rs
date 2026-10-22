//! Highlight detection and virality scoring (LLM over the transcript).
//!
//! Detection runs per overlapping time window so long videos fit the model's
//! context; candidates are snapped to sentence boundaries, length-limited and
//! de-duplicated. Scoring uses a fixed rubric; the total is computed here from
//! the criteria so it is stable and explainable, not whatever the model says.

use hk_stt::{SegmentOpts, Transcript};
use serde::{Deserialize, Serialize};

use crate::error::{Result, StudioError};
use crate::llm::{Llm, complete_json};

/// A candidate short clip in source seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub start: f64,
    pub end: f64,
    pub title: String,
    pub hook: String,
    pub reason: String,
    /// funny, insight, story, emotional, controversial, tip, ...
    pub kind: String,
    /// Model's own 0-100 ranking signal (used to order candidates before scoring).
    pub rank: f64,
}

impl Candidate {
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// A moment the audio side flagged (laughter, energy peak, ...). Optional prompt hint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioHint {
    pub t: f64,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HighlightOpts {
    pub min_secs: f64,
    pub max_secs: f64,
    /// How many clips to return overall.
    pub count: usize,
    pub window_secs: f64,
    pub overlap_secs: f64,
    pub hints: Vec<AudioHint>,
    /// Extra instructions ("only funny moments", "about pricing").
    pub brief: Option<String>,
}

impl Default for HighlightOpts {
    fn default() -> Self {
        Self {
            min_secs: 15.0,
            max_secs: 75.0,
            count: 8,
            window_secs: 600.0,
            overlap_secs: 60.0,
            hints: Vec::new(),
            brief: None,
        }
    }
}

const DETECT_SYSTEM: &str = "You are an expert short-form video editor (TikTok, Reels, Shorts). \
From a timestamped transcript of a long video, pick the most engaging self-contained moments: \
funny, surprising, emotionally strong, useful tips, strong opinions, great stories. \
A clip must start on a hook (not mid-sentence), make sense without the rest of the video, and end on a payoff. \
Times are seconds from the start of the whole video, exactly as in the transcript. \
Reply with ONLY JSON: {\"clips\":[{\"start\":number,\"end\":number,\"title\":string,\"hook\":string,\"reason\":string,\"kind\":string,\"rank\":number(0-100)}]}.";

#[derive(Deserialize)]
struct DetectOut {
    #[serde(default)]
    clips: Vec<RawClip>,
}

#[derive(Deserialize)]
struct RawClip {
    start: f64,
    end: f64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    hook: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    rank: f64,
}

struct Sentences {
    starts: Vec<f64>,
    ends: Vec<f64>,
    lines: Vec<(f64, String)>,
}

fn sentences(tr: &Transcript) -> Sentences {
    let mut t = tr.clone();
    if t.segments.is_empty() {
        t.regroup_segments(&SegmentOpts::default());
    }
    let mut s = Sentences {
        starts: Vec::new(),
        ends: Vec::new(),
        lines: Vec::new(),
    };
    for seg in &t.segments {
        s.starts.push(seg.start);
        s.ends.push(seg.end);
        s.lines.push((seg.start, seg.text.trim().to_string()));
    }
    s
}

fn nearest(points: &[f64], t: f64) -> f64 {
    points
        .iter()
        .copied()
        .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
        .unwrap_or(t)
}

fn overlap(a: &Candidate, b: &Candidate) -> f64 {
    let o = (a.end.min(b.end) - a.start.max(b.start)).max(0.0);
    o / a.duration().min(b.duration()).max(1e-9)
}

/// Find candidate clips, best first.
pub async fn find_highlights(
    llm: &dyn Llm,
    tr: &Transcript,
    opts: &HighlightOpts,
) -> Result<Vec<Candidate>> {
    if tr.words.is_empty() {
        return Err(StudioError::Invalid("empty transcript".into()));
    }
    let sent = sentences(tr);
    let total = tr.duration.max(tr.words.last().map_or(0.0, |w| w.end));
    let step = (opts.window_secs - opts.overlap_secs).max(30.0);
    let mut cands: Vec<Candidate> = Vec::new();
    let mut w0 = 0.0;
    loop {
        let w1 = (w0 + opts.window_secs).min(total + 1.0);
        let lines: Vec<String> = sent
            .lines
            .iter()
            .filter(|(t, _)| *t >= w0 && *t < w1)
            .map(|(t, s)| format!("[{t:.1}] {s}"))
            .collect();
        if !lines.is_empty() {
            let mut user = format!(
                "Video length {total:.0}s. Window {w0:.0}s-{w1:.0}s. Return up to {} clips of {:.0}-{:.0} seconds each, only inside this window.\n",
                opts.count.max(3),
                opts.min_secs,
                opts.max_secs
            );
            if let Some(b) = &opts.brief {
                user.push_str(&format!("Editor's brief: {b}\n"));
            }
            let hints: Vec<String> = opts
                .hints
                .iter()
                .filter(|h| h.t >= w0 && h.t < w1)
                .map(|h| format!("{:.1}s {}", h.t, h.label))
                .collect();
            if !hints.is_empty() {
                user.push_str(&format!("Audio signals (hints): {}\n", hints.join(", ")));
            }
            user.push_str("\nTranscript:\n");
            user.push_str(&lines.join("\n"));
            let out: DetectOut = complete_json(llm, DETECT_SYSTEM, &user).await?;
            for c in out.clips {
                if let Some(c) = clean(c, &sent, opts, w0, w1) {
                    cands.push(c);
                }
            }
        }
        if w1 >= total {
            break;
        }
        w0 += step;
    }
    Ok(select(cands, opts.count))
}

fn clean(
    c: RawClip,
    sent: &Sentences,
    opts: &HighlightOpts,
    w0: f64,
    w1: f64,
) -> Option<Candidate> {
    if !(c.start.is_finite() && c.end.is_finite()) || c.end <= c.start {
        return None;
    }
    let mut start = nearest(&sent.starts, c.start.max(w0));
    let mut end = nearest(&sent.ends, c.end.min(w1));
    if end - start > opts.max_secs {
        // Pull the end back to the last sentence end inside the limit.
        end = sent
            .ends
            .iter()
            .copied()
            .filter(|e| *e > start && *e - start <= opts.max_secs)
            .fold(f64::NEG_INFINITY, f64::max);
        if !end.is_finite() {
            return None;
        }
    }
    if end <= start {
        start = c.start;
        end = c.end;
    }
    if end - start < opts.min_secs {
        return None;
    }
    Some(Candidate {
        start,
        end,
        title: c.title.trim().to_string(),
        hook: c.hook.trim().to_string(),
        reason: c.reason.trim().to_string(),
        kind: if c.kind.is_empty() {
            "highlight".into()
        } else {
            c.kind.to_lowercase()
        },
        rank: c.rank.clamp(0.0, 100.0),
    })
}

/// Best-first greedy pick, dropping candidates that mostly overlap a better one.
fn select(mut cands: Vec<Candidate>, count: usize) -> Vec<Candidate> {
    cands.sort_by(|a, b| b.rank.total_cmp(&a.rank).then(a.start.total_cmp(&b.start)));
    let mut out: Vec<Candidate> = Vec::new();
    for c in cands {
        if out.iter().all(|k| overlap(&c, k) < 0.3) {
            out.push(c);
        }
        if out.len() >= count {
            break;
        }
    }
    out
}

/// Criteria, each 0-100.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViralScore {
    /// Weighted total 0-100 (computed from the criteria).
    pub total: u32,
    pub hook: u32,
    pub emotion: u32,
    pub novelty: u32,
    pub clarity: u32,
    pub pacing: u32,
    pub shareability: u32,
    pub platform_fit: u32,
    pub summary: String,
    pub strengths: Vec<String>,
    pub improvements: Vec<String>,
    /// Hash of (rubric, text); lets callers skip re-scoring unchanged clips.
    pub key: String,
}

/// Weights of (hook, emotion, novelty, clarity, pacing, shareability, platform_fit).
pub const WEIGHTS: [f64; 7] = [0.25, 0.15, 0.10, 0.10, 0.10, 0.20, 0.10];

const SCORE_SYSTEM: &str = "You rate short-form video clips for social-media performance. \
Be critical and calibrated: 50 is average, 80+ is rare. Score each criterion 0-100: \
hook (first 3 seconds grab attention), emotion (humour, awe, anger, curiosity), novelty (fresh or surprising), \
clarity (understandable standalone), pacing (no dead air, tight), shareability (would someone send this), \
platform_fit (fits vertical short-form). This is an estimate, not a prediction guarantee. \
Reply with ONLY JSON: {\"hook\":n,\"emotion\":n,\"novelty\":n,\"clarity\":n,\"pacing\":n,\"shareability\":n,\"platform_fit\":n,\
\"summary\":string,\"strengths\":[string],\"improvements\":[string]}.";

#[derive(Deserialize)]
struct ScoreOut {
    hook: f64,
    emotion: f64,
    novelty: f64,
    clarity: f64,
    pacing: f64,
    shareability: f64,
    platform_fit: f64,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    strengths: Vec<String>,
    #[serde(default)]
    improvements: Vec<String>,
}

fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn score_key(text: &str, rubric: Option<&str>) -> String {
    fnv(&format!("{}\u{1}{text}", rubric.unwrap_or("")))
}

fn clamp100(v: f64) -> u32 {
    if v.is_finite() {
        v.round().clamp(0.0, 100.0) as u32
    } else {
        0
    }
}

/// Score the transcript text of `start..end`. `existing` short-circuits when the key matches.
pub async fn score_clip(
    llm: &dyn Llm,
    tr: &Transcript,
    start: f64,
    end: f64,
    rubric: Option<&str>,
    existing: Option<&ViralScore>,
) -> Result<ViralScore> {
    let text = tr.text_in_range(start, end);
    if text.trim().is_empty() {
        return Err(StudioError::Invalid("clip has no speech to score".into()));
    }
    let key = score_key(&text, rubric);
    if let Some(e) = existing.filter(|e| e.key == key) {
        return Ok(e.clone());
    }
    let mut system = SCORE_SYSTEM.to_string();
    if let Some(r) = rubric {
        system.push_str(&format!("\nExtra rubric from the editor: {r}"));
    }
    let user = format!("Clip length {:.0}s.\nTranscript:\n{text}", end - start);
    let o: ScoreOut = complete_json(llm, &system, &user).await?;
    let parts = [
        o.hook,
        o.emotion,
        o.novelty,
        o.clarity,
        o.pacing,
        o.shareability,
        o.platform_fit,
    ];
    let total = parts
        .iter()
        .zip(WEIGHTS)
        .map(|(p, w)| p.clamp(0.0, 100.0) * w)
        .sum::<f64>();
    Ok(ViralScore {
        total: clamp100(total),
        hook: clamp100(o.hook),
        emotion: clamp100(o.emotion),
        novelty: clamp100(o.novelty),
        clarity: clamp100(o.clarity),
        pacing: clamp100(o.pacing),
        shareability: clamp100(o.shareability),
        platform_fit: clamp100(o.platform_fit),
        summary: o.summary,
        strengths: o.strengths,
        improvements: o.improvements,
        key,
    })
}

#[cfg(test)]
mod tests {
    use hk_stt::Word;

    use super::*;
    use crate::llm::ScriptedLlm;

    /// 20 one-second words per sentence, sentences at 0, 20, 40, ... ending with a period.
    fn long_transcript(sentences: usize) -> Transcript {
        let mut words = Vec::new();
        for s in 0..sentences {
            for k in 0..20 {
                let t = (s * 20 + k) as f64;
                let text = if k == 19 {
                    format!("w{s}.")
                } else {
                    format!("w{s}")
                };
                words.push(Word::new(text, t, t + 0.9));
            }
        }
        Transcript {
            language: Some("en".into()),
            duration: (sentences * 20) as f64,
            words,
            segments: Vec::new(),
        }
    }

    #[tokio::test]
    async fn detects_snaps_and_dedupes() {
        let tr = long_transcript(10); // 200s
        let reply = r#"{"clips":[
            {"start":21.0,"end":79.5,"title":"A","hook":"h","reason":"r","kind":"Funny","rank":90},
            {"start":25.0,"end":70.0,"title":"overlaps A","hook":"","reason":"","kind":"x","rank":80},
            {"start":120.0,"end":125.0,"title":"too short","hook":"","reason":"","kind":"x","rank":99},
            {"start":140.0,"end":190.0,"title":"B","hook":"","reason":"","kind":"tip","rank":70}
        ]}"#;
        let llm = ScriptedLlm::new([reply]);
        let opts = HighlightOpts {
            window_secs: 600.0,
            ..HighlightOpts::default()
        };
        let out = find_highlights(&llm, &tr, &opts).await.unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "A");
        assert_eq!(out[0].kind, "funny");
        assert!(
            (out[0].start - 20.0).abs() < 1e-6 && (out[0].end - 80.0).abs() < 0.2,
            "{:?}",
            out[0]
        );
        assert_eq!(out[1].title, "B");
        assert!(
            out.iter()
                .all(|c| c.duration() >= 15.0 && c.duration() <= 75.0 + 0.2)
        );
    }

    #[tokio::test]
    async fn long_clips_are_trimmed_to_max() {
        let tr = long_transcript(10);
        let llm =
            ScriptedLlm::new([r#"{"clips":[{"start":0,"end":200,"title":"all","rank":50}]}"#]);
        let out = find_highlights(&llm, &tr, &HighlightOpts::default())
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].duration() <= 75.0, "{}", out[0].duration());
    }

    #[tokio::test]
    async fn windows_cover_long_video() {
        let tr = long_transcript(60); // 1200s
        let llm = ScriptedLlm::new([r#"{"clips":[]}"#, r#"{"clips":[]}"#, r#"{"clips":[]}"#]);
        find_highlights(&llm, &tr, &HighlightOpts::default())
            .await
            .unwrap();
        assert!(llm.prompts.lock().unwrap().len() >= 2);
        assert!(llm.prompts.lock().unwrap()[1].1.contains("Window 540s"));
    }

    #[tokio::test]
    async fn score_is_weighted_and_cached() {
        let tr = long_transcript(3);
        let reply = r#"{"hook":80,"emotion":60,"novelty":40,"clarity":100,"pacing":50,"shareability":70,"platform_fit":90,"summary":"ok","strengths":["a"],"improvements":["b"]}"#;
        let llm = ScriptedLlm::new([reply]);
        let s = score_clip(&llm, &tr, 0.0, 40.0, None, None).await.unwrap();
        // 80*.25+60*.15+40*.1+100*.1+50*.1+70*.2+90*.1 = 20+9+4+10+5+14+9 = 71
        assert_eq!(s.total, 71);
        // Same text -> no second LLM call (llm has no replies left).
        let again = score_clip(&llm, &tr, 0.0, 40.0, None, Some(&s))
            .await
            .unwrap();
        assert_eq!(again, s);
        // Different rubric -> different key.
        assert_ne!(score_key("x", None), score_key("x", Some("funny only")));
    }

    #[tokio::test]
    async fn empty_inputs_error() {
        let llm = ScriptedLlm::new(Vec::<String>::new());
        let empty = Transcript {
            language: None,
            duration: 0.0,
            words: vec![],
            segments: vec![],
        };
        assert!(
            find_highlights(&llm, &empty, &HighlightOpts::default())
                .await
                .is_err()
        );
        assert!(
            score_clip(&llm, &empty, 0.0, 10.0, None, None)
                .await
                .is_err()
        );
    }
}
