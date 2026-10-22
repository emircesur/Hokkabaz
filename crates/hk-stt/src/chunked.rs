//! Long-audio wrapper: split into overlapping chunks, transcribe concurrently,
//! stitch by offset and drop the duplicated overlap.
//!
//! Audio cutting is behind [`AudioSplitter`] so this crate does not depend on
//! hk-media; a later crate wires ffmpeg to it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt, stream};

use crate::error::{Result, SttError};
use crate::transcriber::{TranscribeOpts, Transcriber};
use crate::transcript::{Segment, Transcript, Word, normalize_token};

/// Cuts audio. Implemented over ffmpeg elsewhere.
#[async_trait]
pub trait AudioSplitter: Send + Sync {
    /// Duration of `audio` in seconds.
    async fn duration(&self, audio: &Path) -> Result<f64>;
    /// Write `[start, end)` of `audio` to `out` (format from its extension).
    async fn extract(&self, audio: &Path, start: f64, end: f64, out: &Path) -> Result<()>;
}

/// Chunking parameters.
#[derive(Debug, Clone)]
pub struct ChunkOpts {
    /// Target chunk length, seconds.
    pub chunk_secs: f64,
    /// Overlap between consecutive chunks, seconds. Must be < chunk_secs / 2.
    pub overlap_secs: f64,
    /// Max chunks in flight.
    pub concurrency: usize,
    /// Chunk file extension handed to the splitter (`mp3` keeps uploads small).
    pub chunk_ext: String,
    /// Where chunk files go; default is the system temp dir.
    pub work_dir: Option<PathBuf>,
    /// Override the inner transcriber's upload limit.
    pub max_upload_bytes: Option<u64>,
}

impl Default for ChunkOpts {
    fn default() -> Self {
        Self {
            chunk_secs: 600.0,
            overlap_secs: 5.0,
            concurrency: 3,
            chunk_ext: "mp3".into(),
            work_dir: None,
            max_upload_bytes: None,
        }
    }
}

/// Source-time span of each chunk. Last chunk absorbs a tail shorter than the overlap.
pub fn plan_chunks(duration: f64, chunk_secs: f64, overlap_secs: f64) -> Vec<std::ops::Range<f64>> {
    let step = chunk_secs - overlap_secs;
    let mut out = Vec::new();
    if duration <= 0.0 || step <= 0.0 {
        return out;
    }
    let mut start = 0.0;
    loop {
        let mut end = start + chunk_secs;
        if end + overlap_secs >= duration {
            end = duration;
        }
        out.push(start..end);
        if end >= duration {
            break;
        }
        start += step;
    }
    out
}

/// One transcribed chunk with its place on the source timeline.
#[derive(Debug, Clone)]
pub struct ChunkResult {
    /// Chunk start in the source, seconds (offset to add to chunk-relative times).
    pub start: f64,
    /// Chunk end in the source, seconds.
    pub end: f64,
    /// Transcript with chunk-relative times.
    pub transcript: Transcript,
}

/// Same word at nearly the same time on both sides of a cut counts as a duplicate.
const DUP_TOLERANCE: f64 = 0.25;

/// Merge chunk transcripts. Each overlap is cut at its midpoint: earlier chunk
/// owns words/segments centred before it, later chunk the rest. A text+time
/// guard removes the one word that jitter could leave on both sides.
pub fn stitch(parts: Vec<ChunkResult>, total_duration: f64) -> Transcript {
    let n = parts.len();
    let mut out = Transcript { duration: total_duration, ..Default::default() };
    let mut last_chunk = 0usize;
    for (k, part) in parts.iter().enumerate() {
        let mut t = part.transcript.clone();
        t.shift(part.start);
        if out.language.is_none() {
            out.language = t.language.clone();
        }
        let lo = if k == 0 { f64::NEG_INFINITY } else { (part.start + parts[k - 1].end) / 2.0 };
        let hi = if k + 1 == n { f64::INFINITY } else { (parts[k + 1].start + part.end) / 2.0 };
        for w in t.words {
            let m = w.mid();
            if m < lo || m >= hi {
                continue;
            }
            if k != last_chunk && is_dup(out.words.last(), &w) {
                continue;
            }
            last_chunk = k;
            out.words.push(w);
        }
        for s in t.segments {
            let m = (s.start + s.end) / 2.0;
            if m >= lo && m < hi {
                out.segments.push(Segment { word_range: 0..0, ..s });
            }
        }
    }
    if let Some(w) = out.words.last() {
        out.duration = out.duration.max(w.end);
    }
    out.link_segments();
    out
}

fn is_dup(prev: Option<&Word>, w: &Word) -> bool {
    prev.is_some_and(|p| {
        (p.start - w.start).abs() < DUP_TOLERANCE && normalize_token(&p.text) == normalize_token(&w.text)
    })
}

/// Wraps any [`Transcriber`] so arbitrarily long audio fits upload limits.
#[derive(Clone)]
pub struct ChunkedTranscriber {
    inner: Arc<dyn Transcriber>,
    splitter: Arc<dyn AudioSplitter>,
    opts: ChunkOpts,
}

impl std::fmt::Debug for ChunkedTranscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkedTranscriber").field("opts", &self.opts).finish_non_exhaustive()
    }
}

static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

impl ChunkedTranscriber {
    /// Wrap `inner`.
    pub fn new(inner: Arc<dyn Transcriber>, splitter: Arc<dyn AudioSplitter>, opts: ChunkOpts) -> Self {
        Self { inner, splitter, opts }
    }

    fn limit(&self) -> Option<u64> {
        self.opts.max_upload_bytes.or_else(|| self.inner.max_upload_bytes())
    }
}

#[async_trait]
impl Transcriber for ChunkedTranscriber {
    async fn transcribe(&self, audio_path: &Path, opts: &TranscribeOpts) -> Result<Transcript> {
        let o = &self.opts;
        if o.overlap_secs < 0.0 || o.chunk_secs <= 0.0 || o.overlap_secs * 2.0 >= o.chunk_secs {
            return Err(SttError::Config("chunk_secs must exceed 2 * overlap_secs".into()));
        }
        let duration = self.splitter.duration(audio_path).await?;
        let size = tokio::fs::metadata(audio_path)
            .await
            .map_err(|source| SttError::Io { path: audio_path.to_path_buf(), source })?
            .len();
        let fits = self.limit().is_none_or(|l| size <= l);
        if duration <= o.chunk_secs && fits {
            let mut t = self.inner.transcribe(audio_path, opts).await?;
            t.duration = t.duration.max(duration);
            return Ok(t);
        }
        let plan = plan_chunks(duration, o.chunk_secs, o.overlap_secs);
        let dir = o.work_dir.clone().unwrap_or_else(std::env::temp_dir).join(format!(
            "hk-stt-{}-{}",
            std::process::id(),
            DIR_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        tokio::fs::create_dir_all(&dir).await.map_err(|source| SttError::Io { path: dir.clone(), source })?;
        tracing::debug!(chunks = plan.len(), duration, "chunked transcription");

        let res: Result<Vec<ChunkResult>> = stream::iter(plan.into_iter().enumerate())
            .map(|(i, span)| {
                let out = dir.join(format!("chunk-{i:04}.{}", o.chunk_ext));
                async move {
                    self.splitter.extract(audio_path, span.start, span.end, &out).await?;
                    let transcript = self.inner.transcribe(&out, opts).await;
                    let _ = tokio::fs::remove_file(&out).await;
                    Ok(ChunkResult { start: span.start, end: span.end, transcript: transcript? })
                }
            })
            .buffered(o.concurrency.max(1))
            .try_collect()
            .await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        Ok(stitch(res?, duration))
    }

    fn max_upload_bytes(&self) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_covers_duration_with_overlap() {
        let p = plan_chunks(100.0, 40.0, 5.0);
        assert_eq!(p, vec![0.0..40.0, 35.0..75.0, 70.0..100.0]);
        // short tail folded into last chunk
        let p = plan_chunks(78.0, 40.0, 5.0);
        assert_eq!(p, vec![0.0..40.0, 35.0..78.0]);
        assert_eq!(plan_chunks(10.0, 40.0, 5.0), vec![0.0..10.0]);
        assert!(plan_chunks(0.0, 40.0, 5.0).is_empty());
    }
}
