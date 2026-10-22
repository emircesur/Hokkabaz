//! Chunk stitching and ChunkedTranscriber with fake splitter/transcriber.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hk_stt::{
    AudioSplitter, ChunkOpts, ChunkResult, ChunkedTranscriber, Result, TranscribeOpts, Transcriber, Transcript, Word, stitch,
};

/// Words "w0".."wN" every 0.5s across 0..total.
fn words_between(from: f64, to: f64) -> Vec<(String, f64, f64)> {
    let mut v = Vec::new();
    let mut i = (from / 0.5).ceil() as i64;
    while (i as f64) * 0.5 + 0.4 <= to {
        v.push((format!("w{i}"), i as f64 * 0.5, i as f64 * 0.5 + 0.4));
        i += 1;
    }
    v
}

fn chunk(start: f64, end: f64) -> ChunkResult {
    let words = words_between(start, end).into_iter().map(|(t, s, e)| Word::new(t, s - start, e - start)).collect();
    ChunkResult {
        start,
        end,
        transcript: Transcript { language: Some("en".into()), duration: end - start, words, segments: vec![] },
    }
}

#[test]
fn stitch_dedupes_overlap_and_offsets() {
    let t = stitch(vec![chunk(0.0, 10.0), chunk(8.0, 18.0), chunk(16.0, 22.0)], 22.0);
    let texts: Vec<&str> = t.words.iter().map(|w| w.text.as_str()).collect();
    let expect: Vec<String> = words_between(0.0, 22.0).into_iter().map(|w| w.0).collect();
    assert_eq!(texts, expect.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(t.words.windows(2).all(|p| p[0].start < p[1].start));
    assert_eq!(t.language.as_deref(), Some("en"));
    assert!((t.duration - 22.0).abs() < 1e-9);
}

#[test]
fn stitch_removes_jitter_duplicate_at_cut() {
    // Overlap 8..10, cut at 9.0. Word "x" centred just before the cut in chunk 0
    // and just after it in chunk 1 (engine jitter) must appear once.
    let mut a = chunk(0.0, 10.0);
    let mut b = chunk(8.0, 18.0);
    a.transcript.words.retain(|w| w.end < 8.5);
    b.transcript.words.retain(|w| w.start > 1.4); // abs > 9.4
    a.transcript.words.push(Word::new("x", 8.7, 9.0)); // mid 8.85 < cut
    b.transcript.words.insert(0, Word::new("x", 0.9, 1.3)); // abs 8.9..9.3, mid 9.1 >= cut
    let t = stitch(vec![a, b], 18.0);
    assert_eq!(t.words.iter().filter(|w| w.text == "x").count(), 1);
}

#[test]
fn stitch_segments_by_center_and_relinks() {
    let mut a = chunk(0.0, 10.0);
    let mut b = chunk(8.0, 18.0);
    use hk_stt::SegmentOpts;
    a.transcript.regroup_segments(&SegmentOpts { max_words: 5, ..Default::default() });
    b.transcript.regroup_segments(&SegmentOpts { max_words: 5, ..Default::default() });
    let t = stitch(vec![a, b], 18.0);
    assert!(t.segments.windows(2).all(|p| p[0].start < p[1].start));
    let covered: usize = t.segments.iter().map(|s| s.word_range.len()).sum();
    assert!(covered > 0 && covered <= t.words.len());
    assert!(t.segments.windows(2).all(|p| p[0].word_range.end <= p[1].word_range.start));
}

struct FakeSplitter {
    duration: f64,
    calls: Mutex<Vec<(f64, f64)>>,
}

#[async_trait]
impl AudioSplitter for FakeSplitter {
    async fn duration(&self, _audio: &Path) -> Result<f64> {
        Ok(self.duration)
    }
    async fn extract(&self, _audio: &Path, start: f64, end: f64, out: &Path) -> Result<()> {
        self.calls.lock().unwrap().push((start, end));
        // Chunk "audio" = its own span, so the fake transcriber can synthesise words.
        std::fs::write(out, format!("{start} {end}")).unwrap();
        Ok(())
    }
}

struct FakeEngine {
    direct: Mutex<u32>,
}

#[async_trait]
impl Transcriber for FakeEngine {
    async fn transcribe(&self, p: &Path, _o: &TranscribeOpts) -> Result<Transcript> {
        let span = std::fs::read_to_string(p).ok().and_then(|s| {
            let mut it = s.split(' ').map(str::parse::<f64>);
            Some((it.next()?.ok()?, it.next()?.ok()?))
        });
        let Some((a, b)) = span else {
            *self.direct.lock().unwrap() += 1;
            return Ok(Transcript::default());
        };
        Ok(chunk(a, b).transcript)
    }
    fn max_upload_bytes(&self) -> Option<u64> {
        Some(1_000_000)
    }
}

#[tokio::test]
async fn chunked_transcriber_end_to_end_and_short_passthrough() {
    let src = std::env::temp_dir().join(format!("hk-stt-chunked-{}.wav", std::process::id()));
    std::fs::write(&src, b"source").unwrap();
    let engine = Arc::new(FakeEngine { direct: Mutex::new(0) });

    // Long: 30s, 12s chunks, 2s overlap.
    let splitter = Arc::new(FakeSplitter { duration: 30.0, calls: Mutex::new(vec![]) });
    let opts = ChunkOpts { chunk_secs: 12.0, overlap_secs: 2.0, concurrency: 2, ..Default::default() };
    let ct = ChunkedTranscriber::new(engine.clone(), splitter.clone(), opts.clone());
    let t = ct.transcribe(&src, &TranscribeOpts::default()).await.unwrap();
    let expect: Vec<String> = words_between(0.0, 30.0).into_iter().map(|w| w.0).collect();
    assert_eq!(t.words.iter().map(|w| w.text.clone()).collect::<Vec<_>>(), expect);
    let calls = splitter.calls.lock().unwrap().clone();
    assert_eq!(calls, vec![(0.0, 12.0), (10.0, 22.0), (20.0, 30.0)]);
    assert_eq!(*engine.direct.lock().unwrap(), 0);

    // Short: single direct call on the source file.
    let splitter = Arc::new(FakeSplitter { duration: 5.0, calls: Mutex::new(vec![]) });
    let ct = ChunkedTranscriber::new(engine.clone(), splitter.clone(), opts);
    let t = ct.transcribe(&src, &TranscribeOpts::default()).await.unwrap();
    assert!(splitter.calls.lock().unwrap().is_empty());
    assert_eq!(*engine.direct.lock().unwrap(), 1);
    assert!((t.duration - 5.0).abs() < 1e-9);

    // Bad chunk params rejected.
    let bad = ChunkOpts { chunk_secs: 4.0, overlap_secs: 2.0, ..Default::default() };
    let ct = ChunkedTranscriber::new(engine, splitter, bad);
    assert!(ct.transcribe(&src, &TranscribeOpts::default()).await.is_err());
    let _ = std::fs::remove_file(src);
}
