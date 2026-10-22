//! Audio extraction, chunk splitting for long audio, and silence detection.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{MediaError, Result};
use crate::proc::run_capture;
use crate::tools::Tools;

/// PCM wav target. Default is what whisper-class STT expects: 16 kHz mono.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSpec {
    pub sample_rate: u32,
    pub channels: u32,
}

impl Default for AudioSpec {
    fn default() -> Self {
        Self {
            sample_rate: 16_000,
            channels: 1,
        }
    }
}

/// A time span in seconds on the source timeline.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Range {
    pub start: f64,
    pub end: f64,
}

impl Range {
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// One chunk of a split audio file; `offset` is its start on the original timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioChunk {
    pub path: PathBuf,
    pub offset: f64,
    pub duration: f64,
}

fn s(v: &str) -> String {
    v.to_string()
}

fn wav_args(src: &Path, dst: &Path, spec: AudioSpec, window: Option<Range>) -> Vec<String> {
    let mut a = vec![
        s("-hide_banner"),
        s("-nostdin"),
        s("-y"),
        s("-loglevel"),
        s("error"),
    ];
    if let Some(w) = window {
        // Before -i: fast seek, still sample-accurate when transcoding.
        a.extend([
            s("-ss"),
            crate::fmt::num(w.start),
            s("-t"),
            crate::fmt::num(w.duration()),
        ]);
    }
    a.extend([
        s("-i"),
        src.display().to_string(),
        s("-vn"),
        s("-ac"),
        spec.channels.to_string(),
    ]);
    a.extend([
        s("-ar"),
        spec.sample_rate.to_string(),
        s("-c:a"),
        s("pcm_s16le"),
        dst.display().to_string(),
    ]);
    a
}

/// Fixed-length windows with `overlap` seconds shared between neighbours.
/// Overlap lets the caller de-duplicate words cut at chunk borders.
pub fn plan_chunks(total: f64, chunk_len: f64, overlap: f64) -> Result<Vec<Range>> {
    if !(chunk_len.is_finite() && total.is_finite())
        || chunk_len <= 0.0
        || total < 0.0
        || !(0.0..chunk_len).contains(&overlap)
    {
        return Err(MediaError::InvalidSpec(format!(
            "chunking needs chunk_len > overlap >= 0 and total >= 0 (got {chunk_len}, {overlap}, {total})"
        )));
    }
    let mut out = Vec::new();
    let mut start = 0.0;
    loop {
        let end = (start + chunk_len).min(total);
        out.push(Range { start, end });
        if end >= total {
            break;
        }
        start = end - overlap;
    }
    Ok(out)
}

impl Tools {
    /// Decode the audio track of `src` to PCM wav at `dst`.
    pub async fn extract_audio(&self, src: &Path, dst: &Path, spec: AudioSpec) -> Result<()> {
        run_capture(&self.ffmpeg, &wav_args(src, dst, spec, None)).await?;
        Ok(())
    }

    /// Cut `src` into overlapping wav chunks under `dst_dir` (`chunk_0000.wav`, ...).
    /// `total` is the source duration in seconds (use `probe`).
    pub async fn split_audio(
        &self,
        src: &Path,
        dst_dir: &Path,
        spec: AudioSpec,
        total: f64,
        chunk_len: f64,
        overlap: f64,
    ) -> Result<Vec<AudioChunk>> {
        let plan = plan_chunks(total, chunk_len, overlap)?;
        std::fs::create_dir_all(dst_dir)?;
        let mut chunks = Vec::with_capacity(plan.len());
        for (i, w) in plan.into_iter().enumerate() {
            let path = dst_dir.join(format!("chunk_{i:04}.wav"));
            run_capture(&self.ffmpeg, &wav_args(src, &path, spec, Some(w))).await?;
            chunks.push(AudioChunk {
                path,
                offset: w.start,
                duration: w.duration(),
            });
        }
        Ok(chunks)
    }

    /// Silent spans of `src` quieter than `noise_db` (e.g. -30.0) lasting at least `min_dur` seconds.
    pub async fn silencedetect(
        &self,
        src: &Path,
        noise_db: f64,
        min_dur: f64,
    ) -> Result<Vec<Range>> {
        let args = [
            s("-hide_banner"),
            s("-nostdin"),
            s("-nostats"),
            s("-i"),
            src.display().to_string(),
            s("-vn"),
            s("-af"),
            format!(
                "silencedetect=noise={}dB:d={}",
                crate::fmt::num(noise_db),
                crate::fmt::num(min_dur)
            ),
            s("-f"),
            s("null"),
            s("-"),
        ];
        let out = run_capture(&self.ffmpeg, &args).await?;
        Ok(parse_silencedetect(&out.stderr))
    }
}

pub async fn extract_audio(src: &Path, dst: &Path, spec: AudioSpec) -> Result<()> {
    Tools::from_env().extract_audio(src, dst, spec).await
}

pub async fn silencedetect(src: &Path, noise_db: f64, min_dur: f64) -> Result<Vec<Range>> {
    Tools::from_env()
        .silencedetect(src, noise_db, min_dur)
        .await
}

fn after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.split_once(key)?.1.trim_start();
    Some(
        rest.split(|c: char| c.is_whitespace() || c == '|')
            .next()
            .unwrap_or(rest),
    )
}

/// `Duration: HH:MM:SS.xx,` header line.
fn header_duration(line: &str) -> Option<f64> {
    let t = after(line, "Duration:")?.trim_end_matches(',');
    let mut p = t.split(':');
    let (h, m, sec): (f64, f64, f64) = (
        p.next()?.parse().ok()?,
        p.next()?.parse().ok()?,
        p.next()?.parse().ok()?,
    );
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// Parse `silence_start` / `silence_end` lines from ffmpeg stderr.
/// A trailing start with no end (file ends silent) is closed at the header duration.
pub(crate) fn parse_silencedetect(stderr: &str) -> Vec<Range> {
    let mut out = Vec::new();
    let mut open: Option<f64> = None;
    let mut total: Option<f64> = None;
    for line in stderr.lines() {
        if total.is_none() {
            total = header_duration(line);
        }
        if !line.contains("silencedetect") {
            continue;
        }
        if let Some(v) = after(line, "silence_start:").and_then(|v| v.parse::<f64>().ok()) {
            open = Some(v.max(0.0));
        } else if let Some(end) = after(line, "silence_end:").and_then(|v| v.parse::<f64>().ok()) {
            // Missing start means silence from the very beginning.
            out.push(Range {
                start: open.take().unwrap_or(0.0),
                end,
            });
        }
    }
    if let (Some(start), Some(end)) = (open, total) {
        if end > start {
            out.push(Range { start, end });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_overlap_and_cover() {
        let c = plan_chunks(250.0, 100.0, 5.0).unwrap();
        let spans: Vec<(f64, f64)> = c.iter().map(|r| (r.start, r.end)).collect();
        assert_eq!(spans, vec![(0.0, 100.0), (95.0, 195.0), (190.0, 250.0)]);
    }

    #[test]
    fn chunks_short_and_exact() {
        assert_eq!(plan_chunks(30.0, 100.0, 5.0).unwrap().len(), 1);
        let c = plan_chunks(100.0, 100.0, 0.0).unwrap();
        assert_eq!(
            c,
            vec![Range {
                start: 0.0,
                end: 100.0
            }]
        );
    }

    #[test]
    fn chunks_reject_bad_params() {
        assert!(plan_chunks(10.0, 5.0, 5.0).is_err());
        assert!(plan_chunks(10.0, 0.0, 0.0).is_err());
    }

    #[test]
    fn silence_parse() {
        let log = "  Duration: 00:00:10.50, start: 0.000000, bitrate: 1 kb/s\n\
            [silencedetect @ 0x55] silence_start: -0.001\n\
            [silencedetect @ 0x55] silence_end: 1.5 | silence_duration: 1.501\n\
            [silencedetect @ 0x55] silence_start: 8.25\n";
        assert_eq!(
            parse_silencedetect(log),
            vec![
                Range {
                    start: 0.0,
                    end: 1.5
                },
                Range {
                    start: 8.25,
                    end: 10.5
                }
            ]
        );
    }

    #[test]
    fn wav_args_have_seek_before_input() {
        let a = wav_args(
            Path::new("a.mp4"),
            Path::new("o.wav"),
            AudioSpec::default(),
            Some(Range {
                start: 5.0,
                end: 15.5,
            }),
        );
        let ss = a.iter().position(|x| x == "-ss").unwrap();
        let i = a.iter().position(|x| x == "-i").unwrap();
        assert!(ss < i);
        assert_eq!(a[ss + 1], "5");
        assert_eq!(a[ss + 3], "10.5");
        assert!(a.windows(2).any(|w| w == ["-ar", "16000"]));
    }
}
