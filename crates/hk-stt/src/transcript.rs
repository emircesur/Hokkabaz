//! Word-level transcript model plus the helpers text-based editing needs.
//!
//! Times are seconds (f64) on the source media timeline.

use std::collections::BTreeSet;
use std::ops::Range;

use serde::{Deserialize, Serialize};

/// Shortest duration a word may keep after [`Transcript::fix_timings`].
pub const MIN_WORD_DURATION: f64 = 0.02;

/// Per-word markers. All default to false and are omitted when serialising.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WordFlags {
    /// Hesitation / filler (see [`crate::FillerSet`]).
    #[serde(skip_serializing_if = "is_false")]
    pub filler: bool,
    /// Silence gap before this word >= threshold given to [`Transcript::flag_gaps`].
    #[serde(skip_serializing_if = "is_false")]
    pub gap_before: bool,
    /// Silence gap after this word >= threshold.
    #[serde(skip_serializing_if = "is_false")]
    pub gap_after: bool,
    /// Timing was repaired by [`Transcript::fix_timings`].
    #[serde(skip_serializing_if = "is_false")]
    pub retimed: bool,
    /// Time interpolated from a segment (server gave no word times).
    #[serde(skip_serializing_if = "is_false")]
    pub estimated: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One spoken word (or token) with timing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    /// Text as spoken, punctuation attached, no surrounding whitespace.
    pub text: String,
    /// Start, seconds.
    pub start: f64,
    /// End, seconds.
    pub end: f64,
    /// 0..1 when the engine reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// Speaker label when diarised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    /// Markers.
    #[serde(default)]
    pub flags: WordFlags,
}

impl Word {
    /// Plain word, no confidence/speaker/flags.
    pub fn new(text: impl Into<String>, start: f64, end: f64) -> Self {
        Self {
            text: text.into(),
            start,
            end,
            confidence: None,
            speaker: None,
            flags: WordFlags::default(),
        }
    }

    /// `end - start`.
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }

    /// Midpoint, used to assign words to ranges unambiguously.
    pub fn mid(&self) -> f64 {
        (self.start + self.end) / 2.0
    }
}

/// Run of words (sentence / caption block).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// Text of the segment.
    pub text: String,
    /// Start, seconds.
    pub start: f64,
    /// End, seconds.
    pub end: f64,
    /// Indices into [`Transcript::words`] (empty until linked).
    #[serde(default)]
    pub word_range: Range<usize>,
}

/// Knobs for [`Transcript::regroup_segments`].
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentOpts {
    /// Break when the gap to the next word is at least this long.
    pub max_gap: f64,
    /// Break when a segment would exceed this duration.
    pub max_duration: f64,
    /// Break when a segment would exceed this many words.
    pub max_words: usize,
}

impl Default for SegmentOpts {
    fn default() -> Self {
        Self { max_gap: 0.8, max_duration: 12.0, max_words: 40 }
    }
}

/// Hit from [`Transcript::search`].
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// Matching word indices.
    pub words: Range<usize>,
    /// Start of first matching word.
    pub start: f64,
    /// End of last matching word.
    pub end: f64,
}

/// Full transcript of one media file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// ISO-639-1 code when known.
    #[serde(default)]
    pub language: Option<String>,
    /// Media duration, seconds.
    #[serde(default)]
    pub duration: f64,
    /// Words in time order.
    #[serde(default)]
    pub words: Vec<Word>,
    /// Sentence/caption groups over `words`.
    #[serde(default)]
    pub segments: Vec<Segment>,
}

/// Lowercase and strip edge punctuation for matching.
pub(crate) fn normalize_token(s: &str) -> String {
    s.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

fn ends_sentence(text: &str) -> bool {
    let t = text.trim_end_matches(['"', '\'', ')', ']', '”', '’', '»']);
    t.ends_with(['.', '!', '?', '…', '。', '！', '？', '।'])
}

impl Transcript {
    /// All words joined by single spaces.
    pub fn plain_text(&self) -> String {
        join_words(&self.words)
    }

    /// Shift every time by `delta` seconds.
    pub fn shift(&mut self, delta: f64) {
        for w in &mut self.words {
            w.start += delta;
            w.end += delta;
        }
        for s in &mut self.segments {
            s.start += delta;
            s.end += delta;
        }
    }

    /// Repair engine timing quirks so words are ordered, non-overlapping and
    /// at least [`MIN_WORD_DURATION`] long: swaps/zeroes negative spans, trims
    /// overlaps, pushes later words forward when a minimum-length word would
    /// collide. Touched words get `flags.retimed`. Also grows `duration`.
    pub fn fix_timings(&mut self) {
        let mut prev_end = 0.0_f64;
        let mut prev_start = 0.0_f64;
        for i in 0..self.words.len() {
            let w = &mut self.words[i];
            let (os, oe) = (w.start, w.end);
            if !w.start.is_finite() {
                w.start = prev_end;
            }
            if !w.end.is_finite() {
                w.end = w.start;
            }
            if w.end < w.start {
                w.end = w.start;
            }
            if w.start < prev_end {
                // Overlap: trim the previous word if it stays long enough,
                // otherwise push this word forward.
                if i > 0 && w.start - prev_start >= MIN_WORD_DURATION {
                    let ns = w.start;
                    let p = &mut self.words[i - 1];
                    p.end = ns;
                    p.flags.retimed = true;
                } else {
                    let d = w.end - w.start;
                    w.start = prev_end;
                    w.end = w.start + d;
                }
            }
            let w = &mut self.words[i];
            if w.end - w.start < MIN_WORD_DURATION {
                w.end = w.start + MIN_WORD_DURATION;
            }
            if (w.start - os).abs() > 1e-12 || (w.end - oe).abs() > 1e-12 {
                w.flags.retimed = true;
            }
            prev_end = w.end;
            prev_start = w.start;
        }
        if let Some(last) = self.words.last() {
            self.duration = self.duration.max(last.end);
        }
    }

    /// Rebuild `segments` from words: break after sentence-ending punctuation,
    /// long gaps, or max duration/word count.
    pub fn regroup_segments(&mut self, opts: &SegmentOpts) {
        self.segments.clear();
        let n = self.words.len();
        let mut s = 0;
        while s < n {
            let mut e = s;
            while e + 1 < n {
                let cur = &self.words[e];
                let next = &self.words[e + 1];
                let brk = ends_sentence(&cur.text)
                    || next.start - cur.end >= opts.max_gap
                    || next.end - self.words[s].start > opts.max_duration
                    || e + 1 - s >= opts.max_words;
                if brk {
                    break;
                }
                e += 1;
            }
            let ws = &self.words[s..=e];
            self.segments.push(Segment {
                text: join_words(ws),
                start: ws[0].start,
                end: ws[ws.len() - 1].end,
                word_range: s..e + 1,
            });
            s = e + 1;
        }
    }

    /// Point each segment's `word_range` at the words whose midpoint falls in it.
    pub fn link_segments(&mut self) {
        let words = &self.words;
        for seg in &mut self.segments {
            let a = words.partition_point(|w| w.mid() < seg.start);
            let b = words.partition_point(|w| w.mid() < seg.end).max(a);
            seg.word_range = a..b;
        }
    }

    /// Index of the word spoken at `t` (start <= t < end), if any.
    pub fn word_at(&self, t: f64) -> Option<usize> {
        let i = self.words.partition_point(|w| w.start <= t);
        let c = i.checked_sub(1)?;
        (t < self.words[c].end).then_some(c)
    }

    /// Index of the word closest to `t` (the one containing it, else nearest edge).
    pub fn nearest_word(&self, t: f64) -> Option<usize> {
        if self.words.is_empty() {
            return None;
        }
        if let Some(i) = self.word_at(t) {
            return Some(i);
        }
        let i = self.words.partition_point(|w| w.start <= t);
        let before = i.checked_sub(1);
        let after = (i < self.words.len()).then_some(i);
        match (before, after) {
            (Some(b), Some(a)) => {
                let db = t - self.words[b].end;
                let da = self.words[a].start - t;
                Some(if db <= da { b } else { a })
            }
            (Some(b), None) => Some(b),
            (None, a) => a,
        }
    }

    /// Indices of words whose midpoint lies in `[start, end)`.
    pub fn word_range_in(&self, start: f64, end: f64) -> Range<usize> {
        let a = self.words.partition_point(|w| w.mid() < start);
        let b = self.words.partition_point(|w| w.mid() < end).max(a);
        a..b
    }

    /// Text spoken in `[start, end)` (words by midpoint).
    pub fn text_in_range(&self, start: f64, end: f64) -> String {
        join_words(&self.words[self.word_range_in(start, end)])
    }

    /// Case/punctuation-insensitive phrase search; returns time spans.
    pub fn search(&self, query: &str) -> Vec<SearchHit> {
        let q: Vec<String> = query.split_whitespace().map(normalize_token).filter(|t| !t.is_empty()).collect();
        if q.is_empty() || q.len() > self.words.len() {
            return Vec::new();
        }
        let norm: Vec<String> = self.words.iter().map(|w| normalize_token(&w.text)).collect();
        let mut hits = Vec::new();
        let mut i = 0;
        while i + q.len() <= norm.len() {
            if norm[i..i + q.len()] == q[..] {
                hits.push(SearchHit {
                    words: i..i + q.len(),
                    start: self.words[i].start,
                    end: self.words[i + q.len() - 1].end,
                });
                i += q.len();
            } else {
                i += 1;
            }
        }
        hits
    }

    /// Set `gap_before` / `gap_after` where the silence between neighbouring
    /// words (or media start/end) is at least `min_gap` seconds.
    pub fn flag_gaps(&mut self, min_gap: f64) {
        let n = self.words.len();
        for i in 0..n {
            let before = self.words[i].start - if i == 0 { 0.0 } else { self.words[i - 1].end };
            let after = if i + 1 < n {
                self.words[i + 1].start - self.words[i].end
            } else if self.duration > 0.0 {
                self.duration - self.words[i].end
            } else {
                0.0
            };
            self.words[i].flags.gap_before = before >= min_gap;
            self.words[i].flags.gap_after = after >= min_gap;
        }
    }

    /// Silent spans (between words, plus leading/trailing) at least `min_gap` long.
    pub fn silence_gaps(&self, min_gap: f64) -> Vec<Range<f64>> {
        let mut out = Vec::new();
        let mut prev = 0.0;
        for w in &self.words {
            if w.start - prev >= min_gap {
                out.push(prev..w.start);
            }
            prev = prev.max(w.end);
        }
        if self.duration - prev >= min_gap && !self.words.is_empty() {
            out.push(prev..self.duration);
        }
        out
    }

    /// Indices of words flagged as filler.
    pub fn filler_indices(&self) -> BTreeSet<usize> {
        self.words.iter().enumerate().filter(|(_, w)| w.flags.filler).map(|(i, _)| i).collect()
    }

    /// Time ranges to keep after deleting `deleted` word indices.
    ///
    /// Runs of consecutive kept words merge into one range (the silence between
    /// them is kept). `padding` seconds are added on both sides of each range so
    /// cuts do not clip phonemes, but never past the midpoint of the removed
    /// stretch, so neighbouring ranges cannot overlap. Out-of-range indices are
    /// ignored; everything deleted gives an empty list.
    pub fn kept_ranges(&self, deleted: &BTreeSet<usize>, padding: f64) -> Vec<Range<f64>> {
        let n = self.words.len();
        let pad = if padding.is_finite() { padding.max(0.0) } else { 0.0 };
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        while i < n {
            if deleted.contains(&i) {
                i += 1;
                continue;
            }
            let s = i;
            while i + 1 < n && !deleted.contains(&(i + 1)) {
                i += 1;
            }
            runs.push((s, i));
            i += 1;
        }
        let horizon = self.words.last().map_or(0.0, |w| w.end).max(self.duration);
        let mut out: Vec<Range<f64>> = Vec::new();
        let mut prev_we = f64::NEG_INFINITY;
        for (k, &(s, e)) in runs.iter().enumerate() {
            let ws = self.words[s].start;
            let we = self.words[e].end;
            let lo = if s == 0 {
                0.0
            } else {
                let prev_end = if k == 0 { 0.0 } else { self.words[runs[k - 1].1].end };
                (prev_end.min(ws) + ws) / 2.0
            };
            let hi = if e + 1 == n {
                horizon
            } else {
                let next_start = runs.get(k + 1).map_or(horizon, |r| self.words[r.0].start);
                (we + next_start.max(we)) / 2.0
            };
            let start = (ws - pad).max(lo).min(ws).max(0.0);
            let end = (we + pad).min(hi).max(we);
            // Merge only when nothing audible was removed between the runs
            // (zero-length deleted words); padding alone must not undo a cut.
            match out.last_mut() {
                Some(last) if ws - prev_we <= 1e-9 => last.end = last.end.max(end),
                _ => out.push(start..end),
            }
            prev_we = we;
        }
        out
    }
}

fn join_words(words: &[Word]) -> String {
    let mut s = String::new();
    for w in words {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(&w.text);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(words: &[(&str, f64, f64)], duration: f64) -> Transcript {
        Transcript {
            language: Some("en".into()),
            duration,
            words: words.iter().map(|(t, s, e)| Word::new(*t, *s, *e)).collect(),
            segments: vec![],
        }
    }

    fn del(ix: &[usize]) -> BTreeSet<usize> {
        ix.iter().copied().collect()
    }

    fn approx(a: &Range<f64>, s: f64, e: f64) {
        assert!((a.start - s).abs() < 1e-9 && (a.end - e).abs() < 1e-9, "{a:?} != {s}..{e}");
    }

    fn sample() -> Transcript {
        tr(&[("a", 1.0, 1.5), ("b", 1.5, 2.0), ("c", 3.0, 3.5), ("d", 3.5, 4.0)], 5.0)
    }

    #[test]
    fn kept_none_deleted_is_one_padded_range() {
        let r = sample().kept_ranges(&del(&[]), 0.1);
        assert_eq!(r.len(), 1);
        approx(&r[0], 0.9, 4.1);
    }

    #[test]
    fn kept_all_deleted_empty_and_oob_ignored() {
        let t = sample();
        assert!(t.kept_ranges(&del(&[0, 1, 2, 3]), 0.1).is_empty());
        assert_eq!(t.kept_ranges(&del(&[99]), 0.0).len(), 1);
        assert!(Transcript::default().kept_ranges(&del(&[]), 0.1).is_empty());
    }

    #[test]
    fn kept_middle_delete_splits_with_padding_capped_at_midpoint() {
        // delete b (1.5..2.0): kept a [1.0,1.5], c-d [3.0,4.0]; removed stretch 1.5..3.0
        let r = sample().kept_ranges(&del(&[1]), 0.1);
        assert_eq!(r.len(), 2);
        approx(&r[0], 0.9, 1.6);
        approx(&r[1], 2.9, 4.1);
        // huge padding stops at midpoint of removed stretch (2.25), no overlap
        let r = sample().kept_ranges(&del(&[1]), 10.0);
        assert_eq!(r.len(), 2);
        assert!(r[0].end <= r[1].start);
        approx(&r[0], 0.0, 2.25);
        approx(&r[1], 2.25, 5.0);
    }

    #[test]
    fn kept_edges_and_adjacent_deletes() {
        let t = sample();
        let r = t.kept_ranges(&del(&[0]), 0.0);
        assert_eq!(r.len(), 1);
        approx(&r[0], 1.5, 4.0);
        let r = t.kept_ranges(&del(&[3]), 0.0);
        approx(&r[0], 1.0, 3.5);
        let r = t.kept_ranges(&del(&[1, 2]), 0.0);
        assert_eq!(r.len(), 2);
        approx(&r[0], 1.0, 1.5);
        approx(&r[1], 3.5, 4.0);
    }

    #[test]
    fn kept_zero_length_cut_merges() {
        let t = tr(&[("a", 0.0, 1.0), ("b", 1.0, 1.0), ("c", 1.0, 2.0)], 2.0);
        let r = t.kept_ranges(&del(&[1]), 0.05);
        assert_eq!(r.len(), 1);
        approx(&r[0], 0.0, 2.0);
    }

    #[test]
    fn fix_timings_repairs_zero_negative_overlap() {
        let mut t = tr(&[("a", 1.0, 1.0), ("b", 0.9, 0.5), ("c", 1.2, 1.1), ("d", 2.0, 2.5)], 0.0);
        t.fix_timings();
        for w in &t.words {
            assert!(w.duration() >= MIN_WORD_DURATION - 1e-12, "{w:?}");
        }
        for p in t.words.windows(2) {
            assert!(p[0].end <= p[1].start + 1e-9, "{p:?}");
            assert!(p[0].start <= p[1].start);
        }
        assert!(t.words[0].flags.retimed);
        assert!(!t.words[3].flags.retimed);
        assert!((t.duration - 2.5).abs() < 1e-9);
    }

    #[test]
    fn groups_sentences_and_gaps() {
        let mut t = tr(
            &[("Hi.", 0.0, 0.3), ("How", 0.4, 0.6), ("are", 0.6, 0.8), ("you?", 0.8, 1.0), ("fine", 3.0, 3.3), ("ok", 3.3, 3.5)],
            4.0,
        );
        t.regroup_segments(&SegmentOpts::default());
        assert_eq!(t.segments.len(), 3);
        assert_eq!(t.segments[1].text, "How are you?");
        assert_eq!(t.segments[1].word_range, 1..4);
        assert_eq!(t.segments[2].word_range, 4..6);
        let max = SegmentOpts { max_words: 2, ..Default::default() };
        t.regroup_segments(&max);
        assert!(t.segments.iter().all(|s| s.word_range.len() <= 2));
    }

    #[test]
    fn time_queries() {
        let mut t = sample();
        assert_eq!(t.word_at(1.2), Some(0));
        assert_eq!(t.word_at(2.5), None);
        assert_eq!(t.word_at(0.5), None);
        assert_eq!(t.nearest_word(2.2), Some(1));
        assert_eq!(t.nearest_word(2.9), Some(2));
        assert_eq!(t.nearest_word(0.0), Some(0));
        assert_eq!(t.text_in_range(1.0, 2.0), "a b");
        assert_eq!(t.text_in_range(2.1, 2.9), "");
        t.words[1].text = "Big,".into();
        let h = t.search("big c");
        assert_eq!(h.len(), 1);
        assert_eq!((h[0].start, h[0].end), (1.5, 3.5));
        assert!(t.search("zzz").is_empty());
    }

    #[test]
    fn gap_flags_and_silences() {
        let mut t = sample();
        t.flag_gaps(0.5);
        assert!(t.words[0].flags.gap_before); // 1.0s lead-in
        assert!(t.words[1].flags.gap_after); // 1.0s gap
        assert!(t.words[2].flags.gap_before);
        assert!(t.words[3].flags.gap_after); // 1.0s tail
        assert!(!t.words[0].flags.gap_after);
        let g = t.silence_gaps(0.5);
        assert_eq!(g, vec![0.0..1.0, 2.0..3.0, 4.0..5.0]);
    }

    #[test]
    fn serde_roundtrip_omits_false_flags() {
        let t = sample();
        let s = serde_json::to_string(&t).unwrap();
        assert!(!s.contains("filler"));
        assert_eq!(serde_json::from_str::<Transcript>(&s).unwrap(), t);
    }
}
