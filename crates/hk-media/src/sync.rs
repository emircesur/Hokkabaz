//! Multicam / multi-recorder sync by audio (P9.38): loudness envelopes at 100 Hz,
//! normalized cross-correlation over a bounded lag window.

use std::path::Path;

use crate::error::Result;
use crate::proc::run_bytes;
use crate::tools::Tools;

/// Envelope rate (frames per second).
pub const ENV_RATE: u32 = 100;
const PCM_RATE: u32 = 8000;

/// Mean absolute amplitude per 1/ENV_RATE s window of mono f32 samples at `rate`.
pub fn envelope(samples: &[f32], rate: u32) -> Vec<f32> {
    let win = (rate / ENV_RATE).max(1) as usize;
    samples
        .chunks(win)
        .map(|c| c.iter().map(|x| x.abs()).sum::<f32>() / c.len() as f32)
        .collect()
}

/// Seconds to delay `b` so it lines up with `a` (positive = `b` starts later in its file,
/// i.e. trim `offset` s from `b`'s start). Also returns the correlation peak (0..1).
pub fn best_offset(a: &[f32], b: &[f32], max_shift_s: f64) -> (f64, f32) {
    let norm = |v: &[f32]| {
        let m = v.iter().sum::<f32>() / v.len().max(1) as f32;
        v.iter().map(|x| x - m).collect::<Vec<f32>>()
    };
    let (a, b) = (norm(a), norm(b));
    let max = (max_shift_s * f64::from(ENV_RATE)) as isize;
    let (mut best, mut best_lag) = (f32::MIN, 0isize);
    for lag in -max..=max {
        let (mut dot, mut ea, mut eb) = (0f32, 0f32, 0f32);
        for (i, &x) in a.iter().enumerate() {
            let j = i as isize + lag;
            if j < 0 || j as usize >= b.len() {
                continue;
            }
            let y = b[j as usize];
            dot += x * y;
            ea += x * x;
            eb += y * y;
        }
        if ea > 0.0 && eb > 0.0 {
            let c = dot / (ea.sqrt() * eb.sqrt());
            if c > best {
                best = c;
                best_lag = lag;
            }
        }
    }
    (best_lag as f64 / f64::from(ENV_RATE), best.max(0.0))
}

impl Tools {
    /// Loudness envelope of the first `max_s` seconds of a file's audio.
    pub async fn audio_envelope(&self, src: &Path, max_s: f64) -> Result<Vec<f32>> {
        let args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error", "-t"]
            .iter()
            .map(|s| s.to_string())
            .chain([
                format!("{max_s}"),
                "-i".into(),
                src.display().to_string(),
                "-vn".into(),
                "-ac".into(),
                "1".into(),
                "-ar".into(),
                PCM_RATE.to_string(),
                "-f".into(),
                "f32le".into(),
                "-".into(),
            ])
            .collect();
        let raw = run_bytes(&self.ffmpeg, &args).await?;
        let pcm: Vec<f32> = raw
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        Ok(envelope(&pcm, PCM_RATE))
    }

    /// Offset of `b` relative to `a` from their first `window_s` seconds of audio.
    pub async fn sync_offset(
        &self,
        a: &Path,
        b: &Path,
        window_s: f64,
        max_shift_s: f64,
    ) -> Result<(f64, f32)> {
        let ea = self.audio_envelope(a, window_s).await?;
        let eb = self.audio_envelope(b, window_s + max_shift_s).await?;
        Ok(best_offset(&ea, &eb, max_shift_s))
    }
}

impl Tools {
    /// Mix audio files placed at output times (seconds) into one track (`amix`, no
    /// normalization). Used for dubbing tracks built from per-line TTS clips.
    pub async fn mix_at(&self, clips: &[(std::path::PathBuf, f64)], out: &Path) -> Result<()> {
        if clips.is_empty() {
            return Err(crate::MediaError::InvalidSpec("nothing to mix".into()));
        }
        let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-y", "-loglevel", "error"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        for (p, _) in clips {
            args.push("-i".into());
            args.push(p.display().to_string());
        }
        let mut f = String::new();
        for (i, (_, at)) in clips.iter().enumerate() {
            let ms = (at.max(0.0) * 1000.0).round() as u64;
            f.push_str(&format!("[{i}:a]aresample=48000,adelay={ms}|{ms}[d{i}];"));
        }
        let ins: String = (0..clips.len()).map(|i| format!("[d{i}]")).collect();
        f.push_str(&format!(
            "{ins}amix=inputs={}:normalize=0:duration=longest[a]",
            clips.len()
        ));
        args.extend([
            "-filter_complex".into(),
            f,
            "-map".into(),
            "[a]".into(),
            out.display().to_string(),
        ]);
        crate::proc::run_capture(&self.ffmpeg, &args)
            .await
            .map(drop)
    }
}

/// One stretch of the multicam edit: output `[start, end)` seconds shows `angle`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Shot {
    pub start: f64,
    pub end: f64,
    pub angle: usize,
}

/// Active-speaker switching over time-aligned envelopes (one per angle, at `ENV_RATE`).
///
/// Each angle's loudness is divided by its own mean so a hot mic doesn't win every window.
/// A new angle takes over only when it beats the current one by `margin` and the current shot
/// has lasted `min_shot_s`, which keeps the cut from flickering on crosstalk.
pub fn switch_plan(envs: &[Vec<f32>], hop_s: f64, min_shot_s: f64, margin: f32) -> Vec<Shot> {
    let len = envs.iter().map(Vec::len).min().unwrap_or(0);
    if envs.is_empty() || len == 0 {
        return Vec::new();
    }
    let rate = f64::from(ENV_RATE);
    let hop = ((hop_s * rate) as usize).max(1);
    let norm: Vec<f32> = envs
        .iter()
        .map(|e| (e[..len].iter().sum::<f32>() / len as f32).max(1e-6))
        .collect();
    let mut shots: Vec<Shot> = Vec::new();
    let mut cur = 0usize;
    let mut since = 0usize;
    for (w, start) in (0..len).step_by(hop).enumerate() {
        let end = (start + hop).min(len);
        let score =
            |a: usize| envs[a][start..end].iter().sum::<f32>() / (end - start) as f32 / norm[a];
        let best = (0..envs.len())
            .max_by(|&a, &b| score(a).total_cmp(&score(b)))
            .unwrap_or(0);
        if w == 0 {
            cur = best;
        } else if best != cur
            && score(best) > score(cur) * margin
            && (start - since) as f64 / rate >= min_shot_s
        {
            shots.push(Shot {
                start: since as f64 / rate,
                end: start as f64 / rate,
                angle: cur,
            });
            cur = best;
            since = start;
        }
    }
    shots.push(Shot {
        start: since as f64 / rate,
        end: len as f64 / rate,
        angle: cur,
    });
    shots
}

impl Tools {
    /// Render a multicam edit: each shot is cut from its angle (shifted by that angle's sync
    /// offset), normalized to 1080p30, joined with the concat demuxer, and laid over the
    /// `audio` file's track. Per-shot encodes keep memory flat on hour-long podcasts.
    pub async fn multicam_render(
        &self,
        angles: &[(std::path::PathBuf, f64)],
        shots: &[Shot],
        audio: &Path,
        out: &Path,
        progress: &(dyn Fn(usize, usize) + Send + Sync),
    ) -> Result<()> {
        if shots.is_empty() {
            return Err(crate::MediaError::InvalidSpec("no shots".into()));
        }
        let work = out.with_extension("parts");
        tokio::fs::create_dir_all(&work).await?;
        let mut list = String::new();
        for (i, s) in shots.iter().enumerate() {
            progress(i, shots.len() + 1);
            let (src, off) = angles
                .get(s.angle)
                .ok_or_else(|| crate::MediaError::InvalidSpec(format!("angle {}", s.angle)))?;
            let part = work.join(format!("{i:05}.mp4"));
            let args: Vec<String> = vec![
                "-hide_banner".into(),
                "-nostdin".into(),
                "-y".into(),
                "-loglevel".into(),
                "error".into(),
                "-ss".into(),
                format!("{:.3}", (s.start + off).max(0.0)),
                "-i".into(),
                src.display().to_string(),
                "-t".into(),
                format!("{:.3}", s.end - s.start),
                "-an".into(),
                "-vf".into(),
                "scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30".into(),
                "-c:v".into(),
                "libx264".into(),
                "-preset".into(),
                "veryfast".into(),
                "-crf".into(),
                "20".into(),
                "-pix_fmt".into(),
                "yuv420p".into(),
                part.display().to_string(),
            ];
            crate::proc::run_capture(&self.ffmpeg, &args).await?;
            list.push_str(&format!(
                "file '{}'\n",
                part.display().to_string().replace('\'', "'\\''")
            ));
        }
        progress(shots.len(), shots.len() + 1);
        let list_path = work.join("list.txt");
        tokio::fs::write(&list_path, list).await?;
        let total = shots.last().map_or(0.0, |s| s.end);
        let args: Vec<String> = vec![
            "-hide_banner".into(),
            "-nostdin".into(),
            "-y".into(),
            "-loglevel".into(),
            "error".into(),
            "-f".into(),
            "concat".into(),
            "-safe".into(),
            "0".into(),
            "-i".into(),
            list_path.display().to_string(),
            "-i".into(),
            audio.display().to_string(),
            "-map".into(),
            "0:v".into(),
            "-map".into(),
            "1:a?".into(),
            "-c:v".into(),
            "copy".into(),
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "192k".into(),
            "-t".into(),
            format!("{total:.3}"),
            out.display().to_string(),
        ];
        let r = crate::proc::run_capture(&self.ffmpeg, &args)
            .await
            .map(drop);
        let _ = tokio::fs::remove_dir_all(&work).await;
        r
    }
}

#[cfg(test)]
mod switch_tests {
    use super::*;

    #[test]
    fn switches_to_active_speaker_with_hysteresis() {
        let r = ENV_RATE as usize;
        // Angle 0 talks 0–10 s, angle 1 talks 10–20 s; angle 1's mic is 5x hotter overall.
        let mut a = vec![0.2f32; 20 * r];
        let mut b = vec![0.1f32; 20 * r];
        for x in &mut a[..10 * r] {
            *x = 1.0;
        }
        for x in &mut b[10 * r..] {
            *x = 5.0;
        }
        // A 0.3 s blip on angle 1 inside angle 0's turn must not cut.
        for x in &mut b[4 * r..4 * r + 30] {
            *x = 5.0;
        }
        let shots = switch_plan(&[a, b], 0.5, 2.0, 1.3);
        assert_eq!(shots.len(), 2, "{shots:?}");
        assert_eq!(shots[0].angle, 0);
        assert_eq!(shots[1].angle, 1);
        assert!((shots[1].start - 10.0).abs() < 0.51);
        assert!((shots[1].end - 20.0).abs() < 1e-9);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_known_lag() {
        // Pseudo-random bursts; b is a delayed by 1.23 s.
        let mut x: u32 = 12345;
        let a: Vec<f32> = (0..3000)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((x >> 16) & 0x7fff) as f32 / 32768.0
            })
            .collect();
        let mut b = vec![0.0f32; 123];
        b.extend_from_slice(&a);
        let (off, peak) = best_offset(&a, &b, 3.0);
        assert!((off - 1.23).abs() < 1e-9, "{off}");
        assert!(peak > 0.99);
    }
}
