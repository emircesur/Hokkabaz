//! Subject tracking for reframing: moving objects (always) and faces (`vision`).
//!
//! ffmpeg decodes the clip range to small grayscale frames at a low rate; the
//! trackers turn them into [`FocusPoint`]s that `reframe::plan_crop` smooths.
//! Face detection is the SeetaFace model via `rustface` (pure Rust); the model
//! file is fetched once into the data dir.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use hk_media::Tools;

use crate::error::{Result, StudioError};
use crate::reframe::{FocusPoint, FocusSource};

/// Decoded analysis frames.
#[derive(Debug)]
pub struct Frames {
    pub width: usize,
    pub height: usize,
    pub fps: f64,
    pub start: f64,
    pub data: Vec<Vec<u8>>,
}

const MAX_BYTES: usize = 256 * 1024 * 1024;

/// Decode `start..end` of `video` to `width`-wide gray frames at `fps`.
pub async fn grab_frames(
    tools: &Tools,
    video: &Path,
    start: f64,
    end: f64,
    fps: f64,
    width: usize,
) -> Result<Frames> {
    let info = tools.probe(video).await?;
    let (sw, sh) = info
        .display_size()
        .ok_or_else(|| StudioError::Invalid("no video stream".into()))?;
    let width = width.max(64) & !1;
    let height = ((width as f64 * f64::from(sh) / f64::from(sw)).round() as usize).max(2) & !1;
    let dur = (end - start).max(0.1);
    let frame_bytes = width * height;
    if (dur * fps).ceil() as usize * frame_bytes > MAX_BYTES {
        return Err(StudioError::Invalid(
            "range too long for tracking; analyse clips, not the whole video".into(),
        ));
    }
    #[cfg(target_os = "android")]
    {
        let _ = (tools, video);
        return Err(StudioError::Invalid(
            "tracking needs ffmpeg, not available on Android".into(),
        ));
    }
    #[cfg(not(target_os = "android"))]
    {
        use std::process::Stdio;
        use tokio::io::AsyncReadExt;
        let mut child = tokio::process::Command::new(hk_media::resolve_program(&tools.ffmpeg))
            .args([
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-ss",
                &format!("{start:.3}"),
                "-t",
                &format!("{dur:.3}"),
                "-i",
            ])
            .arg(video)
            .args([
                "-an",
                "-vf",
                &format!("fps={fps},scale={width}:{height},format=gray"),
                "-f",
                "rawvideo",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| StudioError::Invalid(format!("ffmpeg: {e}")))?;
        let mut out = child
            .stdout
            .take()
            .ok_or_else(|| StudioError::Invalid("no ffmpeg stdout".into()))?;
        let mut data = Vec::new();
        loop {
            let mut buf = vec![0u8; frame_bytes];
            match out.read_exact(&mut buf).await {
                Ok(_) => data.push(buf),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
        }
        let _ = child.wait().await;
        Ok(Frames {
            width,
            height,
            fps,
            start,
            data,
        })
    }
}

/// Follows whatever moves most: centroid of frame-to-frame change.
#[derive(Debug, Clone)]
pub struct MotionFocus {
    tools: Tools,
    /// Pixel difference that counts as motion (0-255).
    pub threshold: u8,
    /// Fraction of the frame that must change to move the focus.
    pub min_area: f64,
}

impl MotionFocus {
    pub fn new(tools: Tools) -> Self {
        Self {
            tools,
            threshold: 24,
            min_area: 0.002,
        }
    }

    pub fn points(&self, f: &Frames) -> Vec<FocusPoint> {
        let (w, h) = (f.width, f.height);
        let mut out = Vec::with_capacity(f.data.len());
        let (mut last_x, mut last_y) = (0.5, 0.5);
        for (i, cur) in f.data.iter().enumerate() {
            let t = f.start + i as f64 / f.fps;
            if i > 0 {
                let prev = &f.data[i - 1];
                let (mut sx, mut sy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
                let mut n = 0usize;
                for y in 0..h {
                    for x in 0..w {
                        let d = cur[y * w + x].abs_diff(prev[y * w + x]);
                        if d > self.threshold {
                            let wgt = f64::from(d);
                            sx += x as f64 * wgt;
                            sy += y as f64 * wgt;
                            sw += wgt;
                            n += 1;
                        }
                    }
                }
                if n as f64 >= self.min_area * (w * h) as f64 && sw > 0.0 {
                    last_x = sx / sw / w as f64;
                    last_y = sy / sw / h as f64;
                }
            }
            out.push(FocusPoint {
                t,
                x: last_x,
                y: last_y,
            });
        }
        out
    }
}

#[async_trait]
impl FocusSource for MotionFocus {
    async fn detect(
        &self,
        video: &Path,
        start: f64,
        end: f64,
        step: f64,
    ) -> Result<Vec<FocusPoint>> {
        let frames = grab_frames(&self.tools, video, start, end, 1.0 / step.max(0.1), 320).await?;
        Ok(self.points(&frames))
    }
}

#[cfg(feature = "vision")]
pub use face::{FaceFocus, ensure_face_model};

#[cfg(feature = "vision")]
mod face {
    use super::{Frames, grab_frames};
    use std::path::{Path, PathBuf};

    use async_trait::async_trait;
    use hk_media::Tools;

    use crate::error::{Result, StudioError};
    use crate::reframe::{FocusPoint, FocusSource};
    use crate::vision::MotionFocus;

    const MODEL_URL: &str = "https://raw.githubusercontent.com/atomashpolskiy/rustface/master/model/seeta_fd_frontal_v1.0.bin";
    const MODEL_FILE: &str = "seeta_fd_frontal_v1.0.bin";

    /// Make sure the SeetaFace model exists in `dir`, downloading it once.
    pub async fn ensure_face_model(dir: &Path) -> Result<PathBuf> {
        let path = dir.join(MODEL_FILE);
        if tokio::fs::metadata(&path)
            .await
            .is_ok_and(|m| m.len() > 100_000)
        {
            return Ok(path);
        }
        tokio::fs::create_dir_all(dir).await?;
        let bytes = reqwest::get(MODEL_URL)
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| StudioError::Http(format!("face model download: {e}")))?
            .bytes()
            .await
            .map_err(|e| StudioError::Http(e.to_string()))?;
        if bytes.len() < 100_000 {
            return Err(StudioError::Http(
                "face model download looks truncated".into(),
            ));
        }
        let tmp = path.with_extension("part");
        tokio::fs::write(&tmp, &bytes).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(path)
    }

    /// Follows the largest face, sticking with the same person unless another
    /// face is clearly bigger; falls back to motion when no face is found.
    #[derive(Debug, Clone)]
    pub struct FaceFocus {
        tools: Tools,
        model: PathBuf,
    }

    impl FaceFocus {
        pub fn new(tools: Tools, model: impl Into<PathBuf>) -> Self {
            Self {
                tools,
                model: model.into(),
            }
        }

        fn run(model: &Path, f: &Frames, motion: &[FocusPoint]) -> Result<Vec<FocusPoint>> {
            let mut det = rustface::create_detector(&model.to_string_lossy())
                .map_err(|e| StudioError::Invalid(format!("face model: {e}")))?;
            det.set_min_face_size(((f.width as f64 * 0.025) as u32).max(20));
            det.set_score_thresh(2.0);
            det.set_pyramid_scale_factor(0.8);
            det.set_slide_window_step(4, 4);
            let mut out = Vec::with_capacity(f.data.len());
            let mut cur: Option<(f64, f64, f64)> = None; // x, y, size (normalised)
            for (i, frame) in f.data.iter().enumerate() {
                let t = f.start + i as f64 / f.fps;
                let img = rustface::ImageData::new(frame, f.width as u32, f.height as u32);
                let faces = det.detect(&img);
                let best = faces
                    .iter()
                    .map(|fc| {
                        let b = fc.bbox();
                        let size = f64::from(b.width()) / f.width as f64;
                        let cx = (f64::from(b.x()) + f64::from(b.width()) / 2.0) / f.width as f64;
                        let cy = (f64::from(b.y()) + f64::from(b.height()) / 2.0) / f.height as f64;
                        (cx, cy, size)
                    })
                    .max_by(|a, b| {
                        // Prefer the face closest to the current subject; a much bigger one wins.
                        let score = |c: &(f64, f64, f64)| {
                            let near =
                                cur.map_or(0.0, |p| -((c.0 - p.0).abs() + (c.1 - p.1).abs()));
                            c.2 + near * 0.5
                        };
                        score(a).total_cmp(&score(b))
                    });
                if let Some(face) = best {
                    cur = Some(face);
                }
                match (best, cur) {
                    (Some((x, y, _)), _) => out.push(FocusPoint { t, x, y }),
                    // Face lost this frame: hold the last face briefly, else motion.
                    (None, Some((x, y, _))) if i % 8 != 0 => out.push(FocusPoint { t, x, y }),
                    _ => {
                        let m = motion
                            .get(i)
                            .copied()
                            .unwrap_or(FocusPoint { t, x: 0.5, y: 0.5 });
                        out.push(FocusPoint { t, x: m.x, y: m.y });
                    }
                }
            }
            Ok(out)
        }
    }

    #[async_trait]
    impl FocusSource for FaceFocus {
        async fn detect(
            &self,
            video: &Path,
            start: f64,
            end: f64,
            step: f64,
        ) -> Result<Vec<FocusPoint>> {
            let frames =
                grab_frames(&self.tools, video, start, end, 1.0 / step.max(0.1), 800).await?;
            let motion = MotionFocus::new(self.tools.clone()).points(&frames);
            let model = self.model.clone();
            tokio::task::spawn_blocking(move || Self::run(&model, &frames, &motion))
                .await
                .map_err(|e| StudioError::Invalid(e.to_string()))?
        }
    }
}

/// Best tracker available: faces if a model path is given and `vision` is on, else motion.
pub fn auto_focus(tools: Tools, face_model: Option<PathBuf>) -> Box<dyn FocusSource> {
    #[cfg(feature = "vision")]
    if let Some(m) = face_model {
        return Box::new(FaceFocus::new(tools, m));
    }
    #[cfg(not(feature = "vision"))]
    let _ = face_model;
    Box::new(MotionFocus::new(tools))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(w: usize, h: usize, boxes: &[(usize, usize)]) -> Frames {
        let data = boxes
            .iter()
            .map(|&(bx, by)| {
                let mut f = vec![10u8; w * h];
                for y in by..(by + 8).min(h) {
                    for x in bx..(bx + 8).min(w) {
                        f[y * w + x] = 250;
                    }
                }
                f
            })
            .collect();
        Frames {
            width: w,
            height: h,
            fps: 2.0,
            start: 10.0,
            data,
        }
    }

    #[test]
    fn motion_centroid_follows_moving_box() {
        let f = frames(80, 40, &[(0, 16), (8, 16), (40, 16), (72, 16)]);
        let pts = MotionFocus::new(Tools::from_env()).points(&f);
        assert_eq!(pts.len(), 4);
        assert!((pts[0].t - 10.0).abs() < 1e-9 && (pts[3].t - 11.5).abs() < 1e-9);
        // Frame 0 has no previous frame: centre. Later frames move right.
        assert_eq!(pts[0].x, 0.5);
        assert!(pts[2].x > pts[1].x && pts[3].x > pts[2].x, "{pts:?}");
    }

    #[test]
    fn still_scene_holds_last_focus() {
        let f = frames(80, 40, &[(10, 10), (10, 10), (10, 10)]);
        let pts = MotionFocus::new(Tools::from_env()).points(&f);
        assert!(pts.iter().all(|p| p.x == 0.5 && p.y == 0.5));
    }
}
