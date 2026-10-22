//! Clip plan → kept source ranges, and the source↔output time map.
//!
//! The ranges come from the clip bounds minus deleted words, fillers and long
//! pauses. Captions, crop keyframes and overlays are expressed on the OUTPUT
//! timeline (after cuts), so [`OutputMap`] converts between the two.

use std::ops::Range;

use hk_stt::Transcript;

use crate::project::{Clip, SilenceCut};

/// Seconds of audio kept around each cut so phonemes are not clipped.
pub const CUT_PAD: f64 = 0.04;

/// Source ranges to keep for `clip`, in order, non-overlapping.
///
/// Consecutive kept words stay joined unless a deleted word sits between them
/// or (with silence removal) the pause exceeds `min_gap`. Every cut gets a
/// small pad on both sides, never more than half the removed stretch.
pub fn kept_ranges(clip: &Clip, tr: &Transcript) -> Vec<Range<f64>> {
    let silence: Option<SilenceCut> = clip.silence;
    let dropped = |i: usize| clip.deleted_words.contains(&i) || (clip.remove_fillers && tr.words[i].flags.filler);

    // Raw runs of kept words: (start, end) clamped to the clip.
    let mut runs: Vec<(f64, f64)> = Vec::new();
    let mut prev: Option<usize> = None;
    for (i, w) in tr.words.iter().enumerate() {
        if w.end <= clip.start || w.start >= clip.end || dropped(i) {
            continue;
        }
        let (ws, we) = (w.start.max(clip.start), w.end.min(clip.end));
        let joined = prev.is_some_and(|p| p + 1 == i && silence.is_none_or(|s| ws - tr.words[p].end <= s.min_gap));
        match (joined, runs.last_mut()) {
            (true, Some(last)) => last.1 = we,
            _ => runs.push((ws, we)),
        }
        prev = Some(i);
    }

    let pad = silence.map_or(CUT_PAD, |s| s.keep);
    let n = runs.len();
    let mut out: Vec<Range<f64>> = Vec::with_capacity(n);
    for k in 0..n {
        let (s, e) = runs[k];
        let left = if k == 0 { (s - clip.start).max(0.0) } else { (s - runs[k - 1].1).max(0.0) / 2.0 };
        let right = if k + 1 == n { (clip.end - e).max(0.0) } else { (runs[k + 1].0 - e).max(0.0) / 2.0 };
        out.push((s - pad.min(left))..(e + pad.min(right)));
    }
    out.retain(|r| r.end - r.start > 0.05);
    out
}

/// Maps source time to output time through a list of kept ranges.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputMap {
    ranges: Vec<Range<f64>>,
    /// Output start of each range.
    offsets: Vec<f64>,
}

impl OutputMap {
    pub fn new(ranges: Vec<Range<f64>>) -> Self {
        let mut offsets = Vec::with_capacity(ranges.len());
        let mut acc = 0.0;
        for r in &ranges {
            offsets.push(acc);
            acc += r.end - r.start;
        }
        Self { ranges, offsets }
    }

    pub fn ranges(&self) -> &[Range<f64>] {
        &self.ranges
    }

    pub fn duration(&self) -> f64 {
        self.ranges.iter().map(|r| r.end - r.start).sum()
    }

    /// Output time of source time `t`, if `t` is inside a kept range.
    pub fn to_output(&self, t: f64) -> Option<f64> {
        self.ranges.iter().zip(&self.offsets).find(|(r, _)| t >= r.start && t <= r.end).map(|(r, o)| o + (t - r.start))
    }

    /// Output time of `t`, snapping removed time to the next kept instant.
    pub fn to_output_snapped(&self, t: f64) -> f64 {
        for (r, o) in self.ranges.iter().zip(&self.offsets) {
            if t < r.start {
                return *o;
            }
            if t <= r.end {
                return o + (t - r.start);
            }
        }
        self.duration()
    }

    /// Source time of output time `t`.
    pub fn to_source(&self, t: f64) -> f64 {
        for (r, o) in self.ranges.iter().zip(&self.offsets) {
            if t <= o + (r.end - r.start) {
                return r.start + (t - o).max(0.0);
            }
        }
        self.ranges.last().map_or(0.0, |r| r.end)
    }
}

#[cfg(test)]
mod tests {
    use hk_stt::{Transcript, Word};

    use super::*;

    fn tr(words: &[(&str, f64, f64)]) -> Transcript {
        Transcript {
            language: Some("en".into()),
            duration: words.last().map_or(0.0, |w| w.2),
            words: words.iter().map(|(t, s, e)| Word::new(*t, *s, *e)).collect(),
            segments: Vec::new(),
        }
    }

    fn clip(start: f64, end: f64) -> Clip {
        Clip::new("c1", "t", start, end)
    }

    #[test]
    fn untouched_clip_is_one_range() {
        let t = tr(&[("a", 0.0, 0.5), ("b", 0.6, 1.0), ("c", 1.1, 1.6)]);
        let r = kept_ranges(&clip(0.0, 2.0), &t);
        assert_eq!(r.len(), 1);
        assert!(r[0].start <= 0.0 + 1e-9 && r[0].end >= 1.6);
    }

    #[test]
    fn deleted_word_splits_range() {
        let t = tr(&[("a", 0.0, 0.5), ("b", 0.6, 1.0), ("c", 1.1, 1.6)]);
        let mut c = clip(0.0, 2.0);
        c.deleted_words.insert(1);
        let r = kept_ranges(&c, &t);
        assert_eq!(r.len(), 2);
        assert!(r[0].end <= 0.6 && r[1].start >= 0.5);
        assert!(r[0].end <= r[1].start);
    }

    #[test]
    fn long_pause_is_cut_with_silence_removal() {
        let t = tr(&[("a", 0.0, 0.5), ("b", 3.0, 3.4)]);
        let mut c = clip(0.0, 4.0);
        assert_eq!(kept_ranges(&c, &t).len(), 1);
        c.silence = Some(SilenceCut { min_gap: 0.6, keep: 0.1 });
        let r = kept_ranges(&c, &t);
        assert_eq!(r.len(), 2);
        let total: f64 = r.iter().map(|x| x.end - x.start).sum();
        assert!(total < 1.5, "pause removed, got {total}");
    }

    #[test]
    fn fillers_dropped_only_when_enabled() {
        let mut t = tr(&[("so", 0.0, 0.3), ("um", 0.4, 0.7), ("yes", 0.8, 1.2)]);
        t.words[1].flags.filler = true;
        let mut c = clip(0.0, 2.0);
        assert_eq!(kept_ranges(&c, &t).len(), 1);
        c.remove_fillers = true;
        assert_eq!(kept_ranges(&c, &t).len(), 2);
    }

    #[test]
    fn clip_bounds_clamp_words() {
        let t = tr(&[("a", 0.0, 1.0), ("b", 1.5, 2.5), ("c", 3.0, 4.0)]);
        let r = kept_ranges(&clip(1.4, 2.6), &t);
        assert_eq!(r.len(), 1);
        assert!(r[0].start >= 1.4 && r[0].end <= 2.6 + 1e-9);
    }

    #[test]
    fn output_map_roundtrip() {
        let m = OutputMap::new(vec![1.0..3.0, 5.0..6.0]);
        assert!((m.duration() - 3.0).abs() < 1e-9);
        assert_eq!(m.to_output(1.5), Some(0.5));
        assert_eq!(m.to_output(5.5), Some(2.5));
        assert_eq!(m.to_output(4.0), None);
        assert!((m.to_output_snapped(4.0) - 2.0).abs() < 1e-9);
        assert!((m.to_source(2.5) - 5.5).abs() < 1e-9);
    }
}
