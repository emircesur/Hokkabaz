//! Import → transcribe → analyze → preview / render.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hk_media::{
    AudioSpec, Ducking, MusicMix, Overlay, OverlayKind, RenderHandle, RenderSpec, Segment, Source,
    Tools, VideoCodec,
};
use hk_stt::{AudioSplitter, FillerSet, SegmentOpts, TranscribeOpts, Transcriber};
use serde::{Deserialize, Serialize};

use crate::broll::{AssetKind, BrollPlacement};
use crate::captions::{CapWord, CaptionStyle, to_ass, words_for_clip};
use crate::edl::{OutputMap, kept_ranges};
use crate::error::{Result, StudioError};
use crate::highlights::{HighlightOpts, find_highlights, score_clip};
use crate::llm::Llm;
use crate::project::{Clip, ProjectStore, StudioProject, probe_source};
use crate::reframe::plan_crop;
use crate::templates::{apply_template, builtin};

/// Title of the whole-video clip every import starts with.
pub const FULL_VIDEO: &str = "Full video";

/// A transcript word as the live editor shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewWord {
    pub idx: usize,
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub deleted: bool,
    pub filler: bool,
    /// Position on the output timeline; `None` if the word is cut.
    pub out_start: Option<f64>,
    pub out_end: Option<f64>,
}

/// Everything a player needs to show the edit WITHOUT rendering: play the kept
/// `ranges` back to back, draw `captions`, apply the `crop` path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewPlan {
    pub clip_id: String,
    pub src_start: f64,
    pub src_end: f64,
    pub duration: f64,
    /// Kept source ranges (start, end), in playback order.
    pub ranges: Vec<(f64, f64)>,
    pub words: Vec<PreviewWord>,
    pub captions: Vec<CapWord>,
    pub caption_style: Option<CaptionStyle>,
    pub crop: Option<hk_media::CropPath>,
    pub out_size: (u32, u32),
    pub broll: Vec<BrollPlacement>,
}

pub fn preview(p: &StudioProject, clip_id: &str) -> Result<PreviewPlan> {
    let clip = p.clip(clip_id)?;
    let tr = p.transcript.as_ref();
    let ranges = match tr {
        Some(t) => kept_ranges(clip, t),
        None => vec![clip.start..clip.end],
    };
    let map = OutputMap::new(ranges.clone());
    let mut words = Vec::new();
    let mut captions = Vec::new();
    if let Some(t) = tr {
        for (i, w) in t.words.iter().enumerate() {
            if w.end <= clip.start || w.start >= clip.end {
                continue;
            }
            let deleted =
                clip.deleted_words.contains(&i) || (clip.remove_fillers && w.flags.filler);
            words.push(PreviewWord {
                idx: i,
                text: w.text.clone(),
                start: w.start,
                end: w.end,
                deleted,
                filler: w.flags.filler,
                out_start: if deleted {
                    None
                } else {
                    map.to_output(w.start.max(clip.start))
                },
                out_end: if deleted {
                    None
                } else {
                    map.to_output(w.end.min(clip.end))
                },
            });
        }
        captions = words_for_clip(t, clip, &map);
    }
    let (sw, sh) = (p.source.width, p.source.height);
    let duration = map.duration();
    let (crop, out_size) = match &clip.reframe {
        Some(r) => {
            let mut path = plan_crop(sw, sh, r, &map, &[]);
            if let Some(pi) = &clip.punch_in {
                let ws: Vec<(f64, f64, &str)> = captions
                    .iter()
                    .map(|w| (w.start, w.end, w.text.as_str()))
                    .collect();
                let times = crate::reframe::emphasis_times(&ws);
                path = crate::reframe::apply_punch_in(&path, &times, duration, pi);
            }
            (Some(path), r.size)
        }
        None => (None, (sw, sh)),
    };
    let broll = clip
        .broll
        .iter()
        .filter(|b| b.start < duration)
        .cloned()
        .collect();
    Ok(PreviewPlan {
        clip_id: clip.id.clone(),
        src_start: clip.start,
        src_end: clip.end,
        duration,
        ranges: ranges.iter().map(|r| (r.start, r.end)).collect(),
        words,
        captions,
        caption_style: clip.captions.clone(),
        crop,
        out_size,
        broll,
    })
}

#[derive(Debug, Clone)]
pub struct RenderOpts {
    pub crf: u8,
    pub codec: VideoCodec,
    pub burn_captions: bool,
    pub fps: Option<f64>,
    /// Output directory; default `<project>/renders`.
    pub out_dir: Option<PathBuf>,
    pub audio_cleanup: Option<hk_media::AudioCleanup>,
    pub encoder: hk_media::HwEncoder,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            crf: 20,
            codec: VideoCodec::H264,
            burn_captions: true,
            fps: None,
            out_dir: None,
            audio_cleanup: None,
            encoder: hk_media::HwEncoder::Software,
        }
    }
}

impl RenderOpts {
    /// Overlay optional JSON keys on the defaults: `audio_cleanup` ("podcast" | "social" | object),
    /// `encoder` ("auto" | "software" | "nvenc" | "qsv" | "videotoolbox" | "mediacodec" | "vaapi" | object),
    /// `crf`, `codec`, `fps`, `burn_captions`. Unknown keys are ignored.
    pub async fn from_json(v: &serde_json::Value, tools: &hk_media::Tools) -> Self {
        let mut o = Self::default();
        if let Some(c) = v.get("crf").and_then(|x| x.as_u64()) {
            o.crf = c.min(51) as u8;
        }
        if let Some(c) = v
            .get("codec")
            .and_then(|x| serde_json::from_value(x.clone()).ok())
        {
            o.codec = c;
        }
        if let Some(f) = v.get("fps").and_then(|x| x.as_f64()) {
            o.fps = Some(f);
        }
        if let Some(b) = v.get("burn_captions").and_then(|x| x.as_bool()) {
            o.burn_captions = b;
        }
        o.audio_cleanup = match v.get("audio_cleanup") {
            Some(serde_json::Value::String(p)) if p == "podcast" => {
                Some(hk_media::AudioCleanup::podcast())
            }
            Some(serde_json::Value::String(p)) if p == "social" => {
                Some(hk_media::AudioCleanup::social())
            }
            Some(x @ serde_json::Value::Object(_)) => serde_json::from_value(x.clone()).ok(),
            _ => None,
        };
        o.encoder = match v.get("encoder") {
            Some(serde_json::Value::String(e)) => match e.as_str() {
                "auto" => tools.detect_encoder(o.codec).await,
                "nvenc" => hk_media::HwEncoder::Nvenc,
                "qsv" => hk_media::HwEncoder::Qsv,
                "videotoolbox" => hk_media::HwEncoder::VideoToolbox,
                "mediacodec" => hk_media::HwEncoder::MediaCodec,
                "vaapi" => hk_media::HwEncoder::Vaapi {
                    device: "/dev/dri/renderD128".into(),
                },
                _ => hk_media::HwEncoder::Software,
            },
            Some(x @ serde_json::Value::Object(_)) => {
                serde_json::from_value(x.clone()).unwrap_or_default()
            }
            _ => hk_media::HwEncoder::Software,
        };
        o
    }
}

/// Build the ffmpeg render spec for one clip (writes the ASS file).
pub async fn build_render(
    store: &ProjectStore,
    p: &StudioProject,
    clip_id: &str,
    opts: &RenderOpts,
) -> Result<RenderSpec> {
    let clip = p.clip(clip_id)?;
    let plan = preview(p, clip_id)?;
    if plan.ranges.is_empty() || plan.duration < 0.2 {
        return Err(StudioError::Invalid(
            "nothing left to render: every word is cut".into(),
        ));
    }
    let work = store.work_dir(&p.id);
    let out_dir = opts.out_dir.clone().unwrap_or_else(|| work.join("renders"));
    tokio::fs::create_dir_all(&out_dir).await?;
    let output = out_dir.join(format!("{}.mp4", clip.id));

    let src = Source {
        path: p.source.path.clone(),
        has_audio: p.source.has_audio,
    };
    let segments: Vec<Segment> = plan
        .ranges
        .iter()
        .map(|&(s, e)| Segment {
            source: 0,
            start: s,
            end: e,
        })
        .collect();
    let mut spec = RenderSpec::new(vec![src], segments, output);
    spec.codec = opts.codec;
    spec.rate = hk_media::RateControl::Crf(opts.crf);
    spec.fps = opts.fps;
    spec.audio_cleanup = opts.audio_cleanup.clone();
    spec.encoder = opts.encoder.clone();
    if plan.ranges.len() > 1 {
        spec.audio_crossfade = Some(0.03);
    }
    if let (Some(crop), Some(r)) = (plan.crop.clone(), &clip.reframe) {
        spec.crop = Some(crop);
        spec.size = Some(r.size);
    }
    if opts.burn_captions && !plan.captions.is_empty() {
        if let Some(style) = &clip.captions {
            let branded = p.brand.as_ref().map(|b| b.style_captions(style));
            let ass = to_ass(
                &plan.captions,
                branded.as_ref().unwrap_or(style),
                plan.out_size,
            );
            let dir = work.join("captions");
            tokio::fs::create_dir_all(&dir).await?;
            let path = dir.join(format!("{}.ass", clip.id));
            tokio::fs::write(&path, ass).await?;
            spec.subtitles = Some(path);
        }
    }
    for b in &plan.broll {
        spec.overlays.push(Overlay {
            path: b.asset.clone(),
            kind: if b.kind == AssetKind::Video {
                OverlayKind::Video
            } else {
                OverlayKind::Image
            },
            start: b.start,
            end: b.end.min(plan.duration),
            x: None,
            y: None,
            width: Some(plan.out_size.0),
            opacity: None,
        });
    }
    if let Some(o) = p
        .brand
        .as_ref()
        .and_then(|b| b.logo_overlay(plan.out_size, plan.duration))
    {
        spec.overlays.push(o);
    }
    if let Some(m) = &clip.music {
        spec.music = Some(MusicMix {
            path: m.path.clone(),
            gain: m.gain,
            ducking: m.duck.then(Ducking::default),
            loop_track: true,
        });
    }
    Ok(spec)
}

/// ffmpeg-backed audio cutter for [`hk_stt::ChunkedTranscriber`].
#[derive(Debug, Clone)]
pub struct MediaSplitter {
    tools: Tools,
}

impl MediaSplitter {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }
}

#[async_trait]
impl AudioSplitter for MediaSplitter {
    async fn duration(&self, audio: &Path) -> hk_stt::Result<f64> {
        let info = self
            .tools
            .probe(audio)
            .await
            .map_err(|e| hk_stt::SttError::Audio(e.to_string()))?;
        Ok(info.duration.unwrap_or(0.0))
    }

    async fn extract(&self, audio: &Path, start: f64, end: f64, out: &Path) -> hk_stt::Result<()> {
        #[cfg(target_os = "android")]
        {
            let _ = (audio, start, end, out);
            Err(hk_stt::SttError::Audio(
                "ffmpeg is not available on Android".into(),
            ))
        }
        #[cfg(not(target_os = "android"))]
        {
            let status =
                tokio::process::Command::new(hk_media::resolve_program(&self.tools.ffmpeg))
                    .args([
                        "-hide_banner",
                        "-nostdin",
                        "-y",
                        "-loglevel",
                        "error",
                        "-ss",
                        &format!("{start:.3}"),
                        "-t",
                        &format!("{:.3}", (end - start).max(0.01)),
                        "-i",
                    ])
                    .arg(audio)
                    .args(["-vn", "-ac", "1", "-ar", "16000"])
                    .arg(out)
                    .kill_on_drop(true)
                    .output()
                    .await
                    .map_err(|e| hk_stt::SttError::Audio(e.to_string()))?;
            if status.status.success() {
                Ok(())
            } else {
                Err(hk_stt::SttError::Audio(
                    String::from_utf8_lossy(&status.stderr).trim().to_string(),
                ))
            }
        }
    }
}

/// The pipeline: store + ffmpeg tools.
#[derive(Debug, Clone)]
pub struct Studio {
    pub store: ProjectStore,
    pub tools: Tools,
}

impl Studio {
    pub fn new(root: impl Into<PathBuf>, tools: Tools) -> Self {
        Self {
            store: ProjectStore::new(root),
            tools,
        }
    }

    pub fn splitter(&self) -> Arc<dyn AudioSplitter> {
        Arc::new(MediaSplitter::new(self.tools.clone()))
    }

    pub async fn import(&self, path: &Path, name: Option<&str>) -> Result<StudioProject> {
        if !path.is_file() {
            return Err(StudioError::NotFound(format!("file {}", path.display())));
        }
        let src = probe_source(path).await?;
        let name = name
            .map(str::to_string)
            .or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "Untitled".into());
        let mut p = StudioProject::new(name, src);
        // Plain editing starts right away: the whole video is the first clip, no transcript needed.
        let _ = p.add_clip(FULL_VIDEO, 0.0, p.source.duration);
        self.store.save(&p).await?;
        Ok(p)
    }

    /// Extract audio, transcribe (chunked if the transcriber is), mark fillers and pauses.
    pub async fn transcribe(
        &self,
        p: &mut StudioProject,
        t: &dyn Transcriber,
        opts: &TranscribeOpts,
        aggressive_fillers: bool,
    ) -> Result<()> {
        if !p.source.has_audio {
            return Err(StudioError::Invalid("source has no audio track".into()));
        }
        let work = self.store.work_dir(&p.id);
        tokio::fs::create_dir_all(&work).await?;
        let audio = work.join("audio.wav");
        self.tools
            .extract_audio(&p.source.path, &audio, AudioSpec::default())
            .await?;
        let mut tr = t.transcribe(&audio, opts).await?;
        tr.fix_timings();
        tr.regroup_segments(&SegmentOpts::default());
        tr.flag_gaps(0.5);
        FillerSet::for_transcript(&tr, aggressive_fillers).mark(&mut tr);
        p.transcript = Some(tr);
        self.store.save_transcript(p).await?;
        self.store.save(p).await?;
        Ok(())
    }

    /// Detect highlights, add them as scored clips (best first). Returns the new clip ids.
    pub async fn analyze(
        &self,
        p: &mut StudioProject,
        llm: &dyn Llm,
        opts: &HighlightOpts,
        rubric: Option<&str>,
        template: Option<&str>,
    ) -> Result<Vec<String>> {
        let tr = p.transcript()?.clone();
        let cands = find_highlights(llm, &tr, opts).await?;
        let tpl = match template {
            Some(n) => Some(
                builtin(n).ok_or_else(|| StudioError::Invalid(format!("unknown template {n}")))?,
            ),
            None => None,
        };
        let mut ids = Vec::new();
        for c in cands {
            let score = score_clip(llm, &tr, c.start, c.end, rubric, None)
                .await
                .ok();
            let clip: &mut Clip = p.add_clip(
                if c.title.is_empty() {
                    "Clip".to_string()
                } else {
                    c.title.clone()
                },
                c.start,
                c.end,
            )?;
            clip.hook = (!c.hook.is_empty()).then(|| c.hook.clone());
            clip.reason = (!c.reason.is_empty()).then(|| c.reason.clone());
            clip.score = score;
            if let Some(t) = &tpl {
                apply_template(clip, t);
            }
            ids.push(clip.id.clone());
        }
        // Best score first.
        p.clips
            .sort_by_key(|c| std::cmp::Reverse(c.score.as_ref().map_or(0, |s| s.total)));
        self.store.save(p).await?;
        Ok(ids)
    }

    pub async fn render(
        &self,
        p: &StudioProject,
        clip_id: &str,
        opts: &RenderOpts,
    ) -> Result<RenderHandle> {
        let spec = build_render(&self.store, p, clip_id, opts).await?;
        Ok(self.tools.render(spec).await?)
    }

    /// Small JPEG for the media bin / clip cards.
    pub async fn thumbnail(&self, p: &StudioProject, at: f64) -> Result<PathBuf> {
        let dir = self.store.work_dir(&p.id).join("thumbs");
        tokio::fs::create_dir_all(&dir).await?;
        let dst = dir.join(format!("{:08.1}.jpg", at.max(0.0)));
        if !dst.exists() {
            self.tools.thumbnail(&p.source.path, at, &dst, 360).await?;
        }
        Ok(dst)
    }

    /// Low-res proxy for smooth scrubbing in the editor.
    pub async fn proxy(&self, p: &StudioProject) -> Result<PathBuf> {
        let dst = self.store.work_dir(&p.id).join("proxy.mp4");
        if !dst.exists() {
            tokio::fs::create_dir_all(self.store.work_dir(&p.id)).await?;
            self.tools.proxy(&p.source.path, &dst, 540).await?;
        }
        Ok(dst)
    }
}
