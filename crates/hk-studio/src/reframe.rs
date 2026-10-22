//! Smart reframing: turn "where the subject is" into a smooth crop path.
//!
//! Focus points (from a face / speaker detector, or set by hand) are noisy;
//! the planner behaves like a camera operator: it holds still while the
//! subject stays inside a dead zone, eases toward them when they leave it, and
//! snaps at scene cuts. Output is an `hk_media::CropPath` on the output timeline.

use std::path::Path;

use async_trait::async_trait;
use hk_media::{CropKey, CropPath, Interp};
use serde::{Deserialize, Serialize};

use crate::edl::OutputMap;
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReframeMode {
    /// Fixed centre crop.
    Center,
    /// Follow `focus` points.
    Track,
}

/// Subject position at a source time, normalised to the source frame (0..1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FocusPoint {
    pub t: f64,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReframeSpec {
    /// Target aspect, e.g. (9, 16).
    pub aspect: (u32, u32),
    /// Final pixel size, e.g. (1080, 1920).
    pub size: (u32, u32),
    pub mode: ReframeMode,
    /// 0 = follow tightly, 1 = very slow camera.
    pub smoothing: f64,
    /// >1 crops tighter than the full-height window.
    pub zoom: f64,
    #[serde(default)]
    pub focus: Vec<FocusPoint>,
}

impl ReframeSpec {
    /// 9:16 at 1080x1920, centred.
    pub fn vertical() -> Self {
        Self { aspect: (9, 16), size: (1080, 1920), mode: ReframeMode::Center, smoothing: 0.5, zoom: 1.0, focus: Vec::new() }
    }

    pub fn square() -> Self {
        Self { aspect: (1, 1), size: (1080, 1080), ..Self::vertical() }
    }
}

/// Finds subjects in a video. Implementations: face detector (feature `vision`
/// in a later crate), external services, or manual.
#[async_trait]
pub trait FocusSource: Send + Sync {
    async fn detect(&self, video: &Path, start: f64, end: f64, step: f64) -> Result<Vec<FocusPoint>>;
}

/// Always the frame centre (the fallback when no detector is available).
#[derive(Debug, Clone, Copy, Default)]
pub struct CenterFocus;

#[async_trait]
impl FocusSource for CenterFocus {
    async fn detect(&self, _video: &Path, start: f64, end: f64, step: f64) -> Result<Vec<FocusPoint>> {
        let step = step.max(0.1);
        let mut out = Vec::new();
        let mut t = start;
        while t <= end {
            out.push(FocusPoint { t, x: 0.5, y: 0.5 });
            t += step;
        }
        Ok(out)
    }
}

/// Crop window size in source pixels for `spec` (even numbers).
pub fn crop_size(src_w: u32, src_h: u32, spec: &ReframeSpec) -> (u32, u32) {
    let (aw, ah) = (f64::from(spec.aspect.0.max(1)), f64::from(spec.aspect.1.max(1)));
    let (sw, sh) = (f64::from(src_w), f64::from(src_h));
    let mut h = sh;
    let mut w = h * aw / ah;
    if w > sw {
        w = sw;
        h = w * ah / aw;
    }
    let z = spec.zoom.max(1.0);
    let (w, h) = (w / z, h / z);
    (even(w), even(h))
}

fn even(v: f64) -> u32 {
    ((v.round() as i64) & !1).max(2) as u32
}

fn focus_at(points: &[FocusPoint], t: f64) -> Option<(f64, f64)> {
    let first = points.first()?;
    if t <= first.t {
        return Some((first.x, first.y));
    }
    for w in points.windows(2) {
        if t <= w[1].t {
            let span = (w[1].t - w[0].t).max(1e-9);
            let f = ((t - w[0].t) / span).clamp(0.0, 1.0);
            return Some((w[0].x + (w[1].x - w[0].x) * f, w[0].y + (w[1].y - w[0].y) * f));
        }
    }
    points.last().map(|p| (p.x, p.y))
}

/// Plan the crop path. `scene_cuts` are source times where the camera may snap.
pub fn plan_crop(src_w: u32, src_h: u32, spec: &ReframeSpec, map: &OutputMap, scene_cuts: &[f64]) -> CropPath {
    let (cw, ch) = crop_size(src_w, src_h, spec);
    let (sw, sh) = (f64::from(src_w), f64::from(src_h));
    let (cwf, chf) = (f64::from(cw), f64::from(ch));
    let clamp = |cx: f64, cy: f64| -> (f64, f64) {
        ((cx - cwf / 2.0).clamp(0.0, (sw - cwf).max(0.0)), (cy - chf / 2.0).clamp(0.0, (sh - chf).max(0.0)))
    };
    let dur = map.duration();
    let centre = clamp(sw / 2.0, sh / 2.0);
    if spec.mode == ReframeMode::Center || spec.focus.is_empty() || dur <= 0.0 {
        return CropPath { keys: vec![CropKey { t: 0.0, x: centre.0.round(), y: centre.1.round(), w: cwf, h: chf }], interp: Interp::Linear };
    }

    let mut focus = spec.focus.clone();
    focus.sort_by(|a, b| a.t.total_cmp(&b.t));
    // Camera inertia: higher smoothing = smaller step toward the target each sample.
    let alpha = (1.0 - spec.smoothing.clamp(0.0, 0.98)).max(0.02);
    let dead_x = cwf * 0.12;
    let dead_y = chf * 0.12;
    const STEP: f64 = 0.2;

    let mut keys: Vec<CropKey> = Vec::new();
    let (mut cx, mut cy): (f64, f64) = (sw / 2.0, sh / 2.0);
    let mut started = false;
    let mut prev_src = f64::NEG_INFINITY;
    let mut t = 0.0;
    while t <= dur + 1e-9 {
        let src_t = map.to_source(t);
        let (fx, fy) = focus_at(&focus, src_t).map_or((sw / 2.0, sh / 2.0), |(x, y)| (x * sw, y * sh));
        let cut = scene_cuts.iter().any(|&c| c > prev_src && c <= src_t);
        // A cut in the source, or a jump across removed material, resets the camera.
        let jumped = started && (src_t - prev_src) > STEP * 4.0;
        if !started || cut || jumped {
            cx = fx;
            cy = fy;
            started = true;
        } else {
            if (fx - cx).abs() > dead_x {
                cx += (fx - cx - (fx - cx).signum() * dead_x) * alpha;
            }
            if (fy - cy).abs() > dead_y {
                cy += (fy - cy - (fy - cy).signum() * dead_y) * alpha;
            }
        }
        let (x, y) = clamp(cx, cy);
        keys.push(CropKey { t, x: x.round(), y: y.round(), w: cwf, h: chf });
        prev_src = src_t;
        t += STEP;
    }
    // Drop keys that sit on the straight line between their neighbours.
    let mut slim: Vec<CropKey> = Vec::with_capacity(keys.len());
    for (i, k) in keys.iter().enumerate() {
        let redundant = i > 0
            && i + 1 < keys.len()
            && (keys[i - 1].x - k.x).abs() < 1.0
            && (keys[i + 1].x - k.x).abs() < 1.0
            && (keys[i - 1].y - k.y).abs() < 1.0
            && (keys[i + 1].y - k.y).abs() < 1.0;
        if !redundant {
            slim.push(*k);
        }
    }
    CropPath { keys: slim, interp: Interp::Smooth }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(d: f64) -> OutputMap {
        OutputMap::new(vec![0.0..d])
    }

    #[test]
    fn crop_size_9_16_from_16_9() {
        let s = ReframeSpec::vertical();
        assert_eq!(crop_size(1920, 1080, &s), (608, 1080));
        // Narrow source: width bound.
        assert_eq!(crop_size(500, 1080, &s), (500, 888));
        let z = ReframeSpec { zoom: 2.0, ..ReframeSpec::vertical() };
        assert_eq!(crop_size(1920, 1080, &z), (304, 540));
    }

    #[test]
    fn center_mode_is_single_key() {
        let p = plan_crop(1920, 1080, &ReframeSpec::vertical(), &map(10.0), &[]);
        assert_eq!(p.keys.len(), 1);
        assert_eq!(p.keys[0].x, 656.0);
        assert_eq!(p.keys[0].w, 608.0);
    }

    #[test]
    fn tracking_follows_subject_and_stays_in_frame() {
        let spec = ReframeSpec {
            mode: ReframeMode::Track,
            smoothing: 0.3,
            focus: vec![FocusPoint { t: 0.0, x: 0.2, y: 0.5 }, FocusPoint { t: 5.0, x: 0.2, y: 0.5 }, FocusPoint { t: 6.0, x: 0.8, y: 0.5 }, FocusPoint { t: 20.0, x: 0.8, y: 0.5 }],
            ..ReframeSpec::vertical()
        };
        let p = plan_crop(1920, 1080, &spec, &map(20.0), &[]);
        let first = p.keys.first().unwrap();
        let last = p.keys.last().unwrap();
        assert!(first.x < 400.0, "starts near left: {}", first.x);
        assert!(last.x > 1000.0, "ends near right: {}", last.x);
        assert!(p.keys.iter().all(|k| k.x >= 0.0 && k.x + k.w <= 1920.0));
        // Still while the subject is still.
        let early: Vec<_> = p.keys.iter().filter(|k| k.t < 4.0).map(|k| k.x).collect();
        assert!(early.windows(2).all(|w| (w[0] - w[1]).abs() < 1.0));
    }

    #[test]
    fn scene_cut_snaps_camera() {
        let spec = ReframeSpec {
            mode: ReframeMode::Track,
            smoothing: 0.95,
            focus: vec![FocusPoint { t: 0.0, x: 0.2, y: 0.5 }, FocusPoint { t: 4.99, x: 0.2, y: 0.5 }, FocusPoint { t: 5.0, x: 0.8, y: 0.5 }, FocusPoint { t: 10.0, x: 0.8, y: 0.5 }],
            ..ReframeSpec::vertical()
        };
        let slow = plan_crop(1920, 1080, &spec, &map(10.0), &[]);
        let snap = plan_crop(1920, 1080, &spec, &map(10.0), &[5.0]);
        let at = |p: &CropPath, t: f64| p.keys.iter().find(|k| k.t >= t).map(|k| k.x).unwrap();
        assert!(at(&snap, 5.2) > at(&slow, 5.2) + 200.0);
    }

    #[tokio::test]
    async fn centre_focus_source_covers_range() {
        let pts = CenterFocus.detect(Path::new("x"), 0.0, 1.0, 0.5).await.unwrap();
        assert_eq!(pts.len(), 3);
    }
}
