//! Render spec and deterministic filter_complex builder.
//!
//! Same spec -> byte-identical graph, so goldens in tests pin the output and EDL renders are
//! cacheable by spec hash.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{MediaError, Result};
use crate::fmt::{escape_value, num};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub path: PathBuf,
    /// Set from `probe`. Audio is rendered only if every referenced source has it.
    pub has_audio: bool,
}

impl Source {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            has_audio: true,
        }
    }
}

/// A kept span of one source, in source seconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub source: usize,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interp {
    #[default]
    Linear,
    /// Smoothstep easing between keyframes (no velocity jump at keys).
    Smooth,
}

/// Crop window at a time on the OUTPUT timeline (after cuts), in source pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CropKey {
    pub t: f64,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CropPath {
    pub keys: Vec<CropKey>,
    #[serde(default)]
    pub interp: Interp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayKind {
    Image,
    Video,
}

/// Image/video drawn over the main video while `start..end` (output seconds). Video overlays
/// play from their own t=0 at `start` (B-roll).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    pub path: PathBuf,
    pub kind: OverlayKind,
    pub start: f64,
    pub end: f64,
    /// ffmpeg overlay expressions; default centered.
    pub x: Option<String>,
    pub y: Option<String>,
    /// Scale to this width keeping aspect.
    pub width: Option<u32>,
    pub opacity: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Ducking {
    /// Linear sidechain threshold, 0.001..=1.
    pub threshold: f64,
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
}

impl Default for Ducking {
    fn default() -> Self {
        Self {
            threshold: 0.05,
            ratio: 8.0,
            attack_ms: 20.0,
            release_ms: 300.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MusicMix {
    pub path: PathBuf,
    /// Linear gain applied to the music.
    pub gain: f64,
    /// Duck music under the speech of the main audio.
    pub ducking: Option<Ducking>,
    pub loop_track: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    #[default]
    H264,
    Hevc,
    Av1,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateControl {
    Crf(u8),
    BitrateKbps(u32),
}

impl Default for RateControl {
    fn default() -> Self {
        Self::Crf(20)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderSpec {
    pub sources: Vec<Source>,
    /// Kept ranges in output order (cut + concat).
    pub segments: Vec<Segment>,
    /// Seconds of audio crossfade at each join; video joins are hard cuts.
    pub audio_crossfade: Option<f64>,
    pub crop: Option<CropPath>,
    /// Output (width, height). Required when the crop path changes size.
    pub size: Option<(u32, u32)>,
    pub fps: Option<f64>,
    /// ASS file burned into the video.
    pub subtitles: Option<PathBuf>,
    pub overlays: Vec<Overlay>,
    pub music: Option<MusicMix>,
    pub codec: VideoCodec,
    pub rate: RateControl,
    /// Encoder preset; defaults per codec.
    pub preset: Option<String>,
    pub audio_bitrate_kbps: u32,
    /// Voice cleanup + loudness; `None` leaves audio untouched.
    #[serde(default)]
    pub audio_cleanup: Option<crate::AudioCleanup>,
    #[serde(default)]
    pub encoder: crate::HwEncoder,
    pub output: PathBuf,
}

impl RenderSpec {
    pub fn new(sources: Vec<Source>, segments: Vec<Segment>, output: impl Into<PathBuf>) -> Self {
        Self {
            sources,
            segments,
            audio_crossfade: None,
            crop: None,
            size: None,
            fps: None,
            subtitles: None,
            overlays: Vec::new(),
            music: None,
            codec: VideoCodec::default(),
            rate: RateControl::default(),
            preset: None,
            audio_bitrate_kbps: 160,
            audio_cleanup: None,
            encoder: crate::HwEncoder::default(),
            output: output.into(),
        }
    }

    /// Expected output length in seconds.
    pub fn duration(&self) -> f64 {
        self.segments.iter().map(|s| s.end - s.start).sum()
    }
}

/// Fully built ffmpeg invocation (argv without the binary).
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltCommand {
    pub args: Vec<String>,
    pub filter_complex: String,
    pub duration: f64,
}

fn bad(msg: impl Into<String>) -> MediaError {
    MediaError::InvalidSpec(msg.into())
}

fn even(v: f64) -> u32 {
    (((v / 2.0).round() as i64) * 2).max(2) as u32
}

fn s(v: &str) -> String {
    v.to_string()
}

impl CropPath {
    fn validate(&self) -> Result<()> {
        if self.keys.is_empty() {
            return Err(bad("crop path has no keyframes"));
        }
        for k in &self.keys {
            if !(k.w > 0.0 && k.h > 0.0 && k.t >= 0.0 && k.x.is_finite() && k.y.is_finite()) {
                return Err(bad("crop keyframe needs w,h > 0 and t >= 0"));
            }
        }
        if self.keys.windows(2).any(|w| w[1].t <= w[0].t) {
            return Err(bad("crop keyframe times must be strictly increasing"));
        }
        Ok(())
    }

    fn constant_size(&self) -> bool {
        let k0 = &self.keys[0];
        self.keys
            .iter()
            .all(|k| even(k.w) == even(k0.w) && even(k.h) == even(k0.h))
    }

    /// Interpolated window at output time `t` (clamped outside the key range).
    fn at(&self, t: f64) -> CropKey {
        let ks = &self.keys;
        if t <= ks[0].t {
            return CropKey { t, ..ks[0] };
        }
        for pair in ks.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if t < b.t {
                let mut p = (t - a.t) / (b.t - a.t);
                if self.interp == Interp::Smooth {
                    p = p * p * (3.0 - 2.0 * p);
                }
                let l = |u: f64, v: f64| u + (v - u) * p;
                return CropKey {
                    t,
                    x: l(a.x, b.x),
                    y: l(a.y, b.y),
                    w: l(a.w, b.w),
                    h: l(a.h, b.h),
                };
            }
        }
        CropKey {
            t,
            ..ks[ks.len() - 1]
        }
    }

    /// ffmpeg expression of one coordinate over `tvar`, nested `if(lt(t,..))` of per-interval lerps.
    fn expr(&self, pick: fn(&CropKey) -> f64, tvar: &str) -> String {
        let ks = &self.keys;
        if ks
            .windows(2)
            .all(|w| (pick(&w[0]) - pick(&w[1])).abs() < 1e-9)
        {
            return num(pick(&ks[0]));
        }
        let mut e = num(pick(&ks[ks.len() - 1]));
        for i in (0..ks.len() - 1).rev() {
            let (a, b) = (&ks[i], &ks[i + 1]);
            let (va, vb) = (pick(a), pick(b));
            let seg = if (vb - va).abs() < 1e-9 {
                num(va)
            } else {
                let p = format!("clip(({tvar}-{})/{},0,1)", num(a.t), num(b.t - a.t));
                let f = match self.interp {
                    Interp::Linear => p,
                    Interp::Smooth => format!("({p})*({p})*(3-2*({p}))"),
                };
                format!("{}+({})*({f})", num(va), num(vb - va))
            };
            e = format!("if(lt({tvar},{}),{seg},{e})", num(b.t));
        }
        e
    }
}

struct Graph {
    steps: Vec<String>,
    video: String,
    audio: Option<String>,
}

impl RenderSpec {
    fn validate(&self) -> Result<()> {
        if self.segments.is_empty() {
            return Err(bad("no segments"));
        }
        for g in &self.segments {
            if g.source >= self.sources.len() {
                return Err(bad(format!("segment source {} out of range", g.source)));
            }
            if !(g.start >= 0.0 && g.end > g.start) {
                return Err(bad(format!("segment {}..{} invalid", g.start, g.end)));
            }
        }
        if let Some(d) = self.audio_crossfade {
            let min = self
                .segments
                .iter()
                .map(|g| g.end - g.start)
                .fold(f64::MAX, f64::min);
            if !(d >= 0.0 && d < min) {
                return Err(bad(
                    "audio crossfade must be >= 0 and shorter than every segment",
                ));
            }
        }
        if let Some(p) = &self.crop {
            p.validate()?;
            if !p.constant_size() && self.size.is_none() {
                return Err(bad("crop path with changing size needs an output size"));
            }
        }
        if let Some((w, h)) = self.size {
            if w == 0 || h == 0 {
                return Err(bad("output size must be non-zero"));
            }
        }
        if let Some(f) = self.fps {
            if !f.is_finite() || f <= 0.0 {
                return Err(bad("fps must be > 0"));
            }
        }
        for o in &self.overlays {
            if !(o.start >= 0.0 && o.end > o.start) {
                return Err(bad("overlay range invalid"));
            }
        }
        if let Some(m) = &self.music {
            if let Some(d) = &m.ducking {
                if !(0.0..=1.0).contains(&d.threshold) || d.ratio < 1.0 {
                    return Err(bad("ducking threshold must be 0..=1 and ratio >= 1"));
                }
            }
        }
        Ok(())
    }

    fn music_input(&self) -> usize {
        self.sources.len() + self.overlays.len()
    }

    fn build_graph(&self) -> Result<Graph> {
        self.validate()?;
        let n = self.sources.len();
        let count = self.segments.len();
        let total = self.duration();
        let mut steps: Vec<String> = Vec::new();

        let with_audio = self
            .segments
            .iter()
            .all(|g| self.sources[g.source].has_audio);
        let xf = self
            .audio_crossfade
            .filter(|d| *d > 0.0 && count > 1 && with_audio);

        // An input pad can feed only one filter: fan out sources used by several segments.
        let mut uses = vec![0usize; n];
        for g in &self.segments {
            uses[g.source] += 1;
        }
        let pads = |kind: &str, split: &str, steps: &mut Vec<String>| -> Vec<Vec<String>> {
            (0..n)
                .map(|i| match uses[i] {
                    0 => Vec::new(),
                    1 => vec![format!("{i}:{kind}")],
                    u => {
                        let labels: Vec<String> =
                            (0..u).map(|j| format!("s{i}{kind}{j}")).collect();
                        let outs: String = labels.iter().map(|l| format!("[{l}]")).collect();
                        steps.push(format!("[{i}:{kind}]{split}={u}{outs}"));
                        labels
                    }
                })
                .collect()
        };
        let vpads = pads("v", "split", &mut steps);
        let apads = if with_audio {
            pads("a", "asplit", &mut steps)
        } else {
            Vec::new()
        };
        let mut vnext = vec![0usize; n];
        let mut anext = vec![0usize; n];

        for (i, g) in self.segments.iter().enumerate() {
            let vl = &vpads[g.source][vnext[g.source]];
            vnext[g.source] += 1;
            steps.push(format!(
                "[{vl}]trim=start={}:end={},setpts=PTS-STARTPTS[v{i}]",
                num(g.start),
                num(g.end)
            ));
            if with_audio {
                let al = &apads[g.source][anext[g.source]];
                anext[g.source] += 1;
                // Tail padding feeds the overlap consumed by acrossfade so A/V stay in sync.
                let tail = match xf {
                    Some(d) if i + 1 < count => d,
                    _ => 0.0,
                };
                steps.push(format!(
                    "[{al}]atrim=start={}:end={},asetpts=PTS-STARTPTS[a{i}]",
                    num(g.start),
                    num(g.end + tail)
                ));
            }
        }

        let mut audio: Option<String> = None;
        match (with_audio, xf) {
            (true, None) => {
                let ins: String = (0..count).map(|i| format!("[v{i}][a{i}]")).collect();
                steps.push(format!("{ins}concat=n={count}:v=1:a=1[vc][ac]"));
                audio = Some(s("ac"));
            }
            (true, Some(d)) => {
                let ins: String = (0..count).map(|i| format!("[v{i}]")).collect();
                steps.push(format!("{ins}concat=n={count}:v=1:a=0[vc]"));
                let mut prev = s("a0");
                for i in 1..count {
                    steps.push(format!(
                        "[{prev}][a{i}]acrossfade=d={}:c1=tri:c2=tri[ax{i}]",
                        num(d)
                    ));
                    prev = format!("ax{i}");
                }
                audio = Some(prev);
            }
            (false, _) => {
                let ins: String = (0..count).map(|i| format!("[v{i}]")).collect();
                steps.push(format!("{ins}concat=n={count}:v=1:a=0[vc]"));
            }
        }
        if let (Some(a), Some(chain)) = (
            &audio,
            self.audio_cleanup.as_ref().and_then(|c| c.voice_chain()),
        ) {
            steps.push(format!("[{a}]{chain}[aclean]"));
            audio = Some(s("aclean"));
        }
        let mut cur = s("vc");

        // Crop, then size/fps.
        let mut scaled = false;
        if let Some(path) = &self.crop {
            if path.constant_size() {
                let k0 = &path.keys[0];
                steps.push(format!(
                    "[{cur}]crop=w={}:h={}:x='{}':y='{}'[vcrop]",
                    even(k0.w),
                    even(k0.h),
                    path.expr(|k| k.x, "t"),
                    path.expr(|k| k.y, "t"),
                ));
            } else {
                let (w, h) = self
                    .size
                    .ok_or_else(|| bad("crop path needs output size"))?;
                let mut bounds = vec![0.0];
                let mut inner: Vec<f64> = path
                    .keys
                    .iter()
                    .map(|k| k.t)
                    .filter(|t| *t > 0.0 && *t < total)
                    .collect();
                inner.dedup();
                bounds.extend(inner);
                bounds.push(total);
                let pieces = bounds.len() - 1;
                let outs: String = (0..pieces).map(|i| format!("[c{i}]")).collect();
                steps.push(format!("[{cur}]split={pieces}{outs}"));
                for i in 0..pieces {
                    let (a, b) = (bounds[i], bounds[i + 1]);
                    let mid = path.at((a + b) / 2.0);
                    // Piece-local `t` restarts at 0; shift key times back onto the output timeline.
                    let tvar = if a == 0.0 {
                        s("t")
                    } else {
                        format!("(t+{})", num(a))
                    };
                    steps.push(format!(
                        "[c{i}]trim=start={}:end={},setpts=PTS-STARTPTS,crop=w={}:h={}:x='{}':y='{}',scale={w}:{h},setsar=1[p{i}]",
                        num(a),
                        num(b),
                        even(mid.w),
                        even(mid.h),
                        path.expr(|k| k.x, &tvar),
                        path.expr(|k| k.y, &tvar),
                    ));
                }
                let ins: String = (0..pieces).map(|i| format!("[p{i}]")).collect();
                steps.push(format!("{ins}concat=n={pieces}:v=1:a=0[vcrop]"));
                scaled = true;
            }
            cur = s("vcrop");
        }
        let mut fmt_filters: Vec<String> = Vec::new();
        if let (Some((w, h)), false) = (self.size, scaled) {
            fmt_filters.push(format!("scale={w}:{h}"));
            fmt_filters.push(s("setsar=1"));
        }
        if let Some(f) = self.fps {
            fmt_filters.push(format!("fps={}", num(f)));
        }
        if !fmt_filters.is_empty() {
            steps.push(format!("[{cur}]{}[vfmt]", fmt_filters.join(",")));
            cur = s("vfmt");
        }

        for (j, o) in self.overlays.iter().enumerate() {
            let idx = n + j;
            let mut f: Vec<String> = Vec::new();
            if o.kind == OverlayKind::Video {
                f.push(format!("trim=duration={}", num(o.end - o.start)));
            }
            f.push(format!("setpts=PTS-STARTPTS+{}/TB", num(o.start)));
            if let Some(w) = o.width {
                f.push(format!("scale={w}:-2"));
            }
            if let Some(op) = o.opacity {
                f.push(s("format=yuva420p"));
                f.push(format!("colorchannelmixer=aa={}", num(op)));
            }
            steps.push(format!("[{idx}:v]{}[ov{j}]", f.join(",")));
            steps.push(format!(
                "[{cur}][ov{j}]overlay=x='{}':y='{}':eof_action=pass:enable='between(t,{},{})'[o{j}]",
                o.x.as_deref().unwrap_or("(W-w)/2"),
                o.y.as_deref().unwrap_or("(H-h)/2"),
                num(o.start),
                num(o.end),
            ));
            cur = format!("o{j}");
        }

        if let Some(sub) = &self.subtitles {
            steps.push(format!(
                "[{cur}]ass=filename={}[vsub]",
                escape_value(&sub.display().to_string())
            ));
            cur = s("vsub");
        }

        if let Some(m) = &self.music {
            let mi = self.music_input();
            let gain = num(m.gain);
            const MIX: &str = "amix=inputs=2:duration=first:normalize=0,alimiter=limit=0.95";
            match (&audio, &m.ducking) {
                (Some(voice), Some(d)) => {
                    steps.push(format!("[{voice}]asplit=2[voice][sc]"));
                    steps.push(format!("[{mi}:a]volume={gain}[mus]"));
                    steps.push(format!(
                        "[mus][sc]sidechaincompress=threshold={}:ratio={}:attack={}:release={}[duck]",
                        num(d.threshold),
                        num(d.ratio),
                        num(d.attack_ms),
                        num(d.release_ms)
                    ));
                    steps.push(format!("[voice][duck]{MIX}[amix]"));
                }
                (Some(voice), None) => {
                    steps.push(format!("[{mi}:a]volume={gain}[mus]"));
                    steps.push(format!("[{voice}][mus]{MIX}[amix]"));
                }
                (None, _) => {
                    steps.push(format!(
                        "[{mi}:a]volume={gain},atrim=duration={},asetpts=PTS-STARTPTS[amix]",
                        num(total)
                    ));
                }
            }
            audio = Some(s("amix"));
        }
        if let (Some(a), Some(chain)) = (
            &audio,
            self.audio_cleanup.as_ref().and_then(|c| c.master_chain()),
        ) {
            steps.push(format!("[{a}]{chain}[aout]"));
            audio = Some(s("aout"));
        }
        if let Some(up) = self.encoder.upload_filter() {
            steps.push(format!("[{cur}]{up}[vhw]"));
            cur = s("vhw");
        }

        Ok(Graph {
            steps,
            video: cur,
            audio,
        })
    }

    /// The `-filter_complex` string alone.
    pub fn filter_complex(&self) -> Result<String> {
        Ok(self.build_graph()?.steps.join(";"))
    }

    /// Full ffmpeg argv (no binary) incl. `-progress pipe:1`.
    pub fn build(&self) -> Result<BuiltCommand> {
        let g = self.build_graph()?;
        let total = self.duration();
        let filter = g.steps.join(";");
        let mut a: Vec<String> = [
            "-hide_banner",
            "-nostdin",
            "-y",
            "-loglevel",
            "warning",
            "-nostats",
            "-progress",
            "pipe:1",
        ]
        .iter()
        .map(|x| s(x))
        .collect();
        a.extend(self.encoder.input_args());
        let mut input = |path: &Path, pre: &[&str]| {
            a.extend(pre.iter().map(|x| s(x)));
            a.push(s("-i"));
            a.push(path.display().to_string());
        };
        for src in &self.sources {
            input(&src.path, &[]);
        }
        for o in &self.overlays {
            match o.kind {
                // Looped stills never end; cap at output length.
                OverlayKind::Image => {
                    let t = num(total);
                    input(&o.path, &["-loop", "1", "-t", &t]);
                }
                OverlayKind::Video => input(&o.path, &[]),
            }
        }
        if let Some(m) = &self.music {
            if m.loop_track {
                input(&m.path, &["-stream_loop", "-1"]);
            } else {
                input(&m.path, &[]);
            }
        }
        a.extend([
            s("-filter_complex"),
            filter.clone(),
            s("-map"),
            format!("[{}]", g.video),
        ]);
        match &g.audio {
            Some(l) => a.extend([s("-map"), format!("[{l}]")]),
            None => a.push(s("-an")),
        }
        a.extend(self.codec_args());
        a.push(self.output.display().to_string());
        Ok(BuiltCommand {
            args: a,
            filter_complex: filter,
            duration: total,
        })
    }

    fn codec_args(&self) -> Vec<String> {
        // Unsupported codec for the family falls back to software for that codec.
        let (hw, enc) = match self.encoder.encoder_name(self.codec) {
            Some(e) => (&self.encoder, e),
            None => (
                &crate::HwEncoder::Software,
                crate::HwEncoder::Software
                    .encoder_name(self.codec)
                    .unwrap_or("libx264"),
            ),
        };
        let mut a = vec![s("-c:v"), s(enc)];
        if let Some(p) = self
            .preset
            .clone()
            .or_else(|| hw.default_preset(self.codec).map(s))
        {
            a.extend([s("-preset"), p]);
        }
        match self.rate {
            RateControl::Crf(c) => a.extend(hw.quality_args(c)),
            RateControl::BitrateKbps(k) => a.extend([s("-b:v"), format!("{k}k")]),
        }
        if self.codec == VideoCodec::Hevc {
            a.extend([s("-tag:v"), s("hvc1")]);
        }
        if hw.upload_filter().is_none() {
            a.extend([s("-pix_fmt"), s("yuv420p")]);
        }
        a.extend([
            s("-c:a"),
            s("aac"),
            s("-b:a"),
            format!("{}k", self.audio_bitrate_kbps),
        ]);
        let ext = self
            .output
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if matches!(ext, "mp4" | "m4v" | "mov") {
            a.extend([s("-movflags"), s("+faststart")]);
        }
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(source: usize, start: f64, end: f64) -> Segment {
        Segment { source, start, end }
    }

    fn base() -> RenderSpec {
        RenderSpec::new(
            vec![Source::new("in.mp4")],
            vec![seg(0, 0.0, 2.0), seg(0, 3.5, 6.0)],
            "out.mp4",
        )
    }

    #[test]
    fn golden_plain_cut() {
        assert_eq!(
            base().filter_complex().unwrap(),
            "[0:v]split=2[s0v0][s0v1];[0:a]asplit=2[s0a0][s0a1];\
             [s0v0]trim=start=0:end=2,setpts=PTS-STARTPTS[v0];\
             [s0a0]atrim=start=0:end=2,asetpts=PTS-STARTPTS[a0];\
             [s0v1]trim=start=3.5:end=6,setpts=PTS-STARTPTS[v1];\
             [s0a1]atrim=start=3.5:end=6,asetpts=PTS-STARTPTS[a1];\
             [v0][a0][v1][a1]concat=n=2:v=1:a=1[vc][ac]"
        );
    }

    #[test]
    fn golden_full_stack() {
        let mut sp = base();
        sp.audio_crossfade = Some(0.03);
        sp.size = Some((180, 320));
        sp.fps = Some(30.0);
        sp.crop = Some(CropPath {
            keys: vec![
                CropKey {
                    t: 0.0,
                    x: 0.0,
                    y: 0.0,
                    w: 135.0,
                    h: 240.0,
                },
                CropKey {
                    t: 2.0,
                    x: 100.0,
                    y: 0.0,
                    w: 135.0,
                    h: 240.0,
                },
            ],
            interp: Interp::Linear,
        });
        sp.subtitles = Some(PathBuf::from("/tmp/my subs/cap.ass"));
        sp.overlays = vec![Overlay {
            path: PathBuf::from("broll.mp4"),
            kind: OverlayKind::Video,
            start: 1.0,
            end: 2.5,
            x: None,
            y: Some("H-h-20".into()),
            width: Some(90),
            opacity: Some(0.8),
        }];
        sp.music = Some(MusicMix {
            path: PathBuf::from("bed.mp3"),
            gain: 0.3,
            ducking: Some(Ducking::default()),
            loop_track: true,
        });
        assert_eq!(
            sp.filter_complex().unwrap(),
            "[0:v]split=2[s0v0][s0v1];[0:a]asplit=2[s0a0][s0a1];\
             [s0v0]trim=start=0:end=2,setpts=PTS-STARTPTS[v0];\
             [s0a0]atrim=start=0:end=2.03,asetpts=PTS-STARTPTS[a0];\
             [s0v1]trim=start=3.5:end=6,setpts=PTS-STARTPTS[v1];\
             [s0a1]atrim=start=3.5:end=6,asetpts=PTS-STARTPTS[a1];\
             [v0][v1]concat=n=2:v=1:a=0[vc];\
             [a0][a1]acrossfade=d=0.03:c1=tri:c2=tri[ax1];\
             [vc]crop=w=136:h=240:x='if(lt(t,2),0+(100)*(clip((t-0)/2,0,1)),100)':y='0'[vcrop];\
             [vcrop]scale=180:320,setsar=1,fps=30[vfmt];\
             [1:v]trim=duration=1.5,setpts=PTS-STARTPTS+1/TB,scale=90:-2,format=yuva420p,colorchannelmixer=aa=0.8[ov0];\
             [vfmt][ov0]overlay=x='(W-w)/2':y='H-h-20':eof_action=pass:enable='between(t,1,2.5)'[o0];\
             [o0]ass=filename=/tmp/my subs/cap.ass[vsub];\
             [ax1]asplit=2[voice][sc];[2:a]volume=0.3[mus];\
             [mus][sc]sidechaincompress=threshold=0.05:ratio=8:attack=20:release=300[duck];\
             [voice][duck]amix=inputs=2:duration=first:normalize=0,alimiter=limit=0.95[amix]"
        );
    }

    #[test]
    fn golden_segmented_crop_smooth() {
        let mut sp = RenderSpec::new(vec![Source::new("in.mp4")], vec![seg(0, 0.0, 4.0)], "o.mkv");
        sp.size = Some((100, 200));
        sp.crop = Some(CropPath {
            keys: vec![
                CropKey {
                    t: 0.0,
                    x: 0.0,
                    y: 10.0,
                    w: 100.0,
                    h: 200.0,
                },
                CropKey {
                    t: 2.0,
                    x: 50.0,
                    y: 10.0,
                    w: 200.0,
                    h: 400.0,
                },
            ],
            interp: Interp::Smooth,
        });
        let g = sp.filter_complex().unwrap();
        assert_eq!(
            g,
            "[0:v]trim=start=0:end=4,setpts=PTS-STARTPTS[v0];\
             [0:a]atrim=start=0:end=4,asetpts=PTS-STARTPTS[a0];\
             [v0][a0]concat=n=1:v=1:a=1[vc][ac];\
             [vc]split=2[c0][c1];\
             [c0]trim=start=0:end=2,setpts=PTS-STARTPTS,crop=w=150:h=300:x='if(lt(t,2),0+(50)*((clip((t-0)/2,0,1))*(clip((t-0)/2,0,1))*(3-2*(clip((t-0)/2,0,1)))),50)':y='10',scale=100:200,setsar=1[p0];\
             [c1]trim=start=2:end=4,setpts=PTS-STARTPTS,crop=w=200:h=400:x='if(lt((t+2),2),0+(50)*((clip(((t+2)-0)/2,0,1))*(clip(((t+2)-0)/2,0,1))*(3-2*(clip(((t+2)-0)/2,0,1)))),50)':y='10',scale=100:200,setsar=1[p1];\
             [p0][p1]concat=n=2:v=1:a=0[vcrop]"
        );
    }

    #[test]
    fn no_audio_sources_drop_audio_and_map_an() {
        let mut sp = base();
        sp.sources[0].has_audio = false;
        let b = sp.build().unwrap();
        assert!(!b.filter_complex.contains("atrim"));
        assert!(b.args.contains(&"-an".to_string()));
    }

    #[test]
    fn image_overlay_is_looped_and_capped() {
        let mut sp = base();
        sp.overlays = vec![Overlay {
            path: PathBuf::from("logo.png"),
            kind: OverlayKind::Image,
            start: 0.0,
            end: 1.0,
            x: None,
            y: None,
            width: None,
            opacity: None,
        }];
        let b = sp.build().unwrap();
        let joined = b.args.join(" ");
        assert!(joined.contains("-loop 1 -t 4.5 -i logo.png"), "{joined}");
        assert!((b.duration - 4.5).abs() < 1e-9);
    }

    #[test]
    fn codec_args() {
        let mut sp = base();
        sp.codec = VideoCodec::Hevc;
        sp.rate = RateControl::BitrateKbps(4000);
        let j = sp.build().unwrap().args.join(" ");
        assert!(j.contains("-c:v libx265 -preset medium -b:v 4000k -tag:v hvc1 -pix_fmt yuv420p"));
        assert!(j.ends_with("-movflags +faststart out.mp4"));
    }

    #[test]
    fn rejects_bad_specs() {
        let mut sp = base();
        sp.segments[0].source = 4;
        assert!(sp.build().is_err());
        let mut sp = base();
        sp.audio_crossfade = Some(5.0);
        assert!(sp.build().is_err());
        let mut sp = base();
        sp.crop = Some(CropPath {
            keys: vec![
                CropKey {
                    t: 0.0,
                    x: 0.0,
                    y: 0.0,
                    w: 10.0,
                    h: 10.0,
                },
                CropKey {
                    t: 1.0,
                    x: 0.0,
                    y: 0.0,
                    w: 20.0,
                    h: 20.0,
                },
            ],
            interp: Interp::Linear,
        });
        assert!(
            sp.build().is_err(),
            "size-changing crop without output size"
        );
    }

    #[test]
    fn deterministic() {
        assert_eq!(base().build().unwrap(), base().build().unwrap());
    }
}
