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
