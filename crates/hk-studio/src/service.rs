//! The Studio service: projects, live sessions and background jobs behind one
//! handle. The UI (FFI) and the agent tools both go through it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hk_media::Tools;
use hk_stt::{AudioSplitter, TranscribeOpts, Transcriber};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, oneshot};

use crate::broll::{BrollOpts, StockProvider, fetch_broll, plan_broll};
use crate::captions::words_for_clip;
use crate::edl::{OutputMap, kept_ranges};
use crate::error::{Result, StudioError};
use crate::highlights::HighlightOpts;
use crate::live::{LiveStudio, StudioEvent};
use crate::llm::Llm;
use crate::pipeline::{RenderOpts, Studio};
use crate::project::{Clip, StudioProject, new_id};
use crate::reframe::{FocusSource, ReframeMode, ReframeSpec};
use crate::social::{Platform, SocialOpts, write_social};
use crate::vision::{MotionFocus, auto_focus};

/// What the host app provides: models, keys, stock accounts.
#[async_trait]
pub trait Backends: Send + Sync + std::fmt::Debug {
    /// The language model for highlights, scoring, B-roll and copy.
    async fn llm(&self) -> Result<Arc<dyn Llm>>;
    /// Speech-to-text engine wrapped for long audio with `splitter`.
    async fn transcriber(
        &self,
        splitter: Arc<dyn AudioSplitter>,
    ) -> Result<(Arc<dyn Transcriber>, TranscribeOpts)>;
    /// Stock footage sources the user configured (Pexels / Pixabay keys, local folders).
    fn stock(&self) -> Vec<Arc<dyn StockProvider>> {
        Vec::new()
    }
    /// YouTube OAuth client + refresh token, if the user set them up.
    fn youtube(&self) -> Option<crate::publish::YoutubeCreds> {
        None
    }
    /// Where the face model lives; `None` = motion tracking only.
    fn face_model_dir(&self) -> Option<PathBuf> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub clips: usize,
    pub has_transcript: bool,
    pub created_ms: u64,
}

impl From<&StudioProject> for ProjectSummary {
    fn from(p: &StudioProject) -> Self {
        Self {
            id: p.id.clone(),
            name: p.name.clone(),
            duration: p.source.duration,
            width: p.source.width,
            height: p.source.height,
            clips: p.clips.len(),
            has_transcript: p.transcript.is_some(),
            created_ms: p.created_ms,
        }
    }
}

#[derive(Debug)]
pub struct StudioService {
    studio: Studio,
    backends: Arc<dyn Backends>,
    live: Mutex<HashMap<String, LiveStudio>>,
    jobs: Mutex<HashMap<String, oneshot::Sender<()>>>,
    renders: Arc<Mutex<HashMap<(String, String), PathBuf>>>,
    /// Renders mirrored in the shared job tree (progress, ETA, cancel). Set once at startup.
    job_registry: std::sync::OnceLock<hk_core::JobRegistry>,
}

impl StudioService {
    /// Report renders as jobs in `reg` (first call wins).
    pub fn set_jobs(&self, reg: hk_core::JobRegistry) {
        let _ = self.job_registry.set(reg);
    }

    pub fn new(root: impl Into<PathBuf>, tools: Tools, backends: Arc<dyn Backends>) -> Self {
        Self {
            studio: Studio::new(root, tools),
            backends,
            live: Mutex::default(),
            jobs: Mutex::default(),
            renders: Arc::default(),
            job_registry: std::sync::OnceLock::new(),
        }
    }

    pub fn studio(&self) -> &Studio {
        &self.studio
    }

    pub async fn projects(&self) -> Result<Vec<ProjectSummary>> {
        Ok(self
            .studio
            .store
            .list()
            .await?
            .iter()
            .map(ProjectSummary::from)
            .collect())
    }

    pub async fn import(&self, path: &Path, name: Option<&str>) -> Result<LiveStudio> {
        let p = self.studio.import(path, name).await?;
        let live = LiveStudio::new(self.studio.store.clone(), p.clone());
        self.live.lock().await.insert(p.id, live.clone());
        Ok(live)
    }

    /// The shared live session of a project (loaded on first use).
    pub async fn open(&self, id: &str) -> Result<LiveStudio> {
        let mut map = self.live.lock().await;
        if let Some(l) = map.get(id) {
            return Ok(l.clone());
        }
        let p = self.studio.store.load(id).await?;
        let live = LiveStudio::new(self.studio.store.clone(), p);
        map.insert(id.to_string(), live.clone());
        Ok(live)
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        self.live.lock().await.remove(id);
        self.studio.store.delete(id).await
    }

    /// Transcribe the source; the project stays editable meanwhile.
    pub async fn transcribe(&self, id: &str) -> Result<()> {
        let live = self.open(id).await?;
        let mut snap = live.snapshot().await;
        let (t, opts) = self.backends.transcriber(self.studio.splitter()).await?;
        self.studio
            .transcribe(&mut snap, t.as_ref(), &opts, false)
            .await?;
        let tr = snap
            .transcript
            .ok_or_else(|| StudioError::Invalid("transcription produced nothing".into()))?;
        live.set_transcript(tr).await
    }

    /// Find highlights, add them as scored clips. Returns the new clip ids.
    pub async fn analyze(
        &self,
        id: &str,
        opts: &HighlightOpts,
        rubric: Option<&str>,
        template: Option<&str>,
    ) -> Result<Vec<String>> {
        let live = self.open(id).await?;
        let llm = self.backends.llm().await?;
        let mut snap = live.snapshot().await;
        let before: Vec<String> = snap.clips.iter().map(|c| c.id.clone()).collect();
        self.studio
            .analyze(&mut snap, llm.as_ref(), opts, rubric, template)
            .await?;
        let new: Vec<Clip> = snap
            .clips
            .into_iter()
            .filter(|c| !before.contains(&c.id))
            .collect();
        let ids = new.iter().map(|c| c.id.clone()).collect();
        live.mutate(|p| {
            p.clips.extend(new);
            p.clips
                .sort_by_key(|c| std::cmp::Reverse(c.score.as_ref().map_or(0, |s| s.total)));
        })
        .await?;
        Ok(ids)
    }

    fn focus_source(&self) -> Box<dyn FocusSource> {
        let model = self
            .backends
            .face_model_dir()
            .map(|d| d.join("seeta_fd_frontal_v1.0.bin"))
            .filter(|p| p.is_file());
        auto_focus(self.studio.tools.clone(), model)
    }

    /// Download the face model into the configured dir (once).
    #[cfg(feature = "vision")]
    pub async fn ensure_face_model(&self) -> Result<Option<PathBuf>> {
        match self.backends.face_model_dir() {
            Some(d) => Ok(Some(crate::vision::ensure_face_model(&d).await?)),
            None => Ok(None),
        }
    }

    /// Detect subjects in the clip and switch it to tracked reframing.
    pub async fn track(&self, id: &str, clip_id: &str, base: Option<ReframeSpec>) -> Result<usize> {
        let live = self.open(id).await?;
        let snap = live.snapshot().await;
        let clip = snap.clip(clip_id)?.clone();
        let src = snap.source.path.clone();
        let points = match self
            .focus_source()
            .detect(&src, clip.start, clip.end, 0.5)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "tracking failed, using motion");
                MotionFocus::new(self.studio.tools.clone())
                    .detect(&src, clip.start, clip.end, 0.5)
                    .await?
            }
        };
        let n = points.len();
        let mut spec = clip
            .reframe
            .clone()
            .or(base)
            .unwrap_or_else(ReframeSpec::vertical);
        spec.mode = ReframeMode::Track;
        spec.focus = points;
        let cid = clip_id.to_string();
        live.mutate(move |p| {
            if let Ok(c) = p.clip_mut(&cid) {
                c.reframe = Some(spec);
            }
        })
        .await?;
        Ok(n)
    }

    /// Plan B-roll with the LLM, fetch it from the configured providers.
    pub async fn add_broll(&self, id: &str, clip_id: &str, opts: &BrollOpts) -> Result<usize> {
        let live = self.open(id).await?;
        let snap = live.snapshot().await;
        let clip = snap.clip(clip_id)?;
        let tr = snap.transcript()?;
        let map = OutputMap::new(kept_ranges(clip, tr));
        let words = words_for_clip(tr, clip, &map);
        let providers = self.backends.stock();
        if providers.is_empty() {
            return Err(StudioError::Invalid(
                "no stock footage source configured (Pexels / Pixabay key or a local folder)"
                    .into(),
            ));
        }
        let llm = self.backends.llm().await?;
        let reqs = plan_broll(llm.as_ref(), &words, opts).await?;
        let refs: Vec<&dyn StockProvider> = providers.iter().map(AsRef::as_ref).collect();
        let dir = self.studio.store.work_dir(id).join("broll");
        let placed = fetch_broll(&refs, &reqs, &dir).await;
        let n = placed.len();
        let cid = clip_id.to_string();
        live.mutate(move |p| {
            if let Ok(c) = p.clip_mut(&cid) {
                c.broll = placed;
            }
        })
        .await?;
        Ok(n)
    }

    pub async fn social(
        &self,
        id: &str,
        clip_id: &str,
        platforms: &[Platform],
        opts: &SocialOpts,
    ) -> Result<()> {
        let live = self.open(id).await?;
        let snap = live.snapshot().await;
        let clip = snap.clip(clip_id)?;
        let tr = snap.transcript()?;
        let text = {
            let map = OutputMap::new(kept_ranges(clip, tr));
            words_for_clip(tr, clip, &map)
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let llm = self.backends.llm().await?;
        let pack = write_social(llm.as_ref(), &clip.title, &text, platforms, opts).await?;
        let cid = clip_id.to_string();
        live.mutate(move |p| {
            if let Ok(c) = p.clip_mut(&cid) {
                c.social = Some(pack);
            }
        })
        .await?;
        Ok(())
    }

    /// YouTube chapters for the whole source (empty when the video is too short for 3).
    pub async fn chapters(&self, id: &str) -> Result<Vec<crate::longform::Chapter>> {
        let snap = self.open(id).await?.snapshot().await;
        let llm = self.backends.llm().await?;
        crate::longform::chapters(llm.as_ref(), snap.transcript()?).await
    }

    /// Blog post / newsletter / thread / summary from the whole transcript.
    pub async fn longform(
        &self,
        id: &str,
        kind: crate::longform::LongformKind,
        language: Option<&str>,
    ) -> Result<String> {
        let snap = self.open(id).await?.snapshot().await;
        let llm = self.backends.llm().await?;
        let tr = snap.transcript()?;
        crate::longform::write_longform(llm.as_ref(), &snap.name, &tr.plain_text(), kind, language)
            .await
    }

    /// Score and rewrite the opening of a clip.
    pub async fn hook(&self, id: &str, clip_id: &str) -> Result<crate::longform::HookReport> {
        let snap = self.open(id).await?.snapshot().await;
        let clip = snap.clip(clip_id)?;
        let tr = snap.transcript()?;
        let map = OutputMap::new(kept_ranges(clip, tr));
        let clip_words = words_for_clip(tr, clip, &map);
        let words: Vec<(f64, &str)> = clip_words
            .iter()
            .map(|w| (w.start, w.text.as_str()))
            .collect();
        let llm = self.backends.llm().await?;
        crate::longform::hook_doctor(llm.as_ref(), &clip.title, &words).await
    }

    /// Translated captions for the whole source as SRT under `<work>/captions/<lang>.srt`.
    pub async fn translate_captions(&self, id: &str, target: &str) -> Result<PathBuf> {
        let snap = self.open(id).await?.snapshot().await;
        let llm = self.backends.llm().await?;
        let lines = crate::longform::transcript_lines(snap.transcript()?);
        let out = crate::longform::translate_lines(llm.as_ref(), &lines, target).await?;
        let dir = self.work_dir(id).join("captions");
        tokio::fs::create_dir_all(&dir).await?;
        let safe: String = target
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let path = dir.join(format!(
            "{}.srt",
            if safe.is_empty() { "translated" } else { &safe }
        ));
        tokio::fs::write(&path, crate::longform::to_srt(&out)).await?;
        Ok(path)
    }

    /// Render options from JSON (see [`RenderOpts::from_json`]); `encoder: "auto"` probes ffmpeg.
    pub async fn render_opts(&self, v: &serde_json::Value) -> RenderOpts {
        RenderOpts::from_json(v, &self.studio.tools).await
    }

    /// Start rendering in the background. Progress and completion arrive as events.
    pub async fn start_render(&self, id: &str, clip_id: &str, opts: RenderOpts) -> Result<String> {
        let live = self.open(id).await?;
        let snap = live.snapshot().await;
        let mut handle = self.studio.render(&snap, clip_id, &opts).await?;
        let job = new_id("r");
        let (tx, mut cancel_rx) = oneshot::channel::<()>();
        self.jobs.lock().await.insert(job.clone(), tx);
        let (pid, cid) = (id.to_string(), clip_id.to_string());
        let mut progress = handle.progress();
        let events = live.clone();
        let renders_key = (pid, cid.clone());
        let renders = self.renders.clone();
        let jh = self.job_registry.get().map(|reg| {
            reg.start(
                hk_core::JobSpec::new(hk_core::JobKind::Render, format!("render {clip_id}"))
                    .key(format!("render:{job}"))
                    .meta(serde_json::json!({ "project": id, "clip": clip_id, "render_job": job })),
                None,
            )
        });
        let job_cancel = jh.as_ref().map(|j| j.token().clone()).unwrap_or_default();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = progress.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let p = *progress.borrow();
                        events.emit(StudioEvent::RenderProgress { clip: cid.clone(), fraction: p.fraction });
                        if let Some(j) = &jh {
                            j.progress(hk_core::Progress::Determinate(p.fraction as f32));
                        }
                        if p.finished {
                            break;
                        }
                    }
                    _ = &mut cancel_rx => {
                        handle.cancel();
                        break;
                    }
                    () = job_cancel.cancelled() => {
                        handle.cancel();
                        break;
                    }
                }
            }
            let res = handle.wait().await;
            if let Some(j) = jh {
                match &res {
                    Ok(_) => j.finish(),
                    Err(_) if j.is_cancelled() => j.cancelled(),
                    Err(e) => j.fail(e.to_string()),
                }
            }
            match res {
                Ok(r) => {
                    renders.lock().await.insert(renders_key, r.output.clone());
                    events.emit(StudioEvent::RenderDone {
                        clip: cid,
                        path: r.output.to_string_lossy().into_owned(),
                    });
                }
                Err(e) => events.emit(StudioEvent::RenderFailed {
                    clip: cid,
                    error: e.to_string(),
                }),
            }
        });
        Ok(job)
    }

    /// Scratch folder of a project (proxies, renders, Concat projects).
    pub fn work_dir(&self, id: &str) -> PathBuf {
        self.studio.store.work_dir(id)
    }

    /// Record `path` as the clip's rendered video (an export made outside the Studio pipeline).
    pub async fn set_render(&self, id: &str, clip: &str, path: PathBuf) {
        self.renders
            .lock()
            .await
            .insert((id.to_string(), clip.to_string()), path);
    }

    pub async fn cancel_render(&self, job: &str) -> bool {
        match self.jobs.lock().await.remove(job) {
            Some(tx) => tx.send(()).is_ok(),
            None => false,
        }
    }

    pub async fn render_path(&self, id: &str, clip: &str) -> Option<PathBuf> {
        let known = self
            .renders
            .lock()
            .await
            .get(&(id.to_string(), clip.to_string()))
            .cloned();
        if known.is_some() {
            return known;
        }
        let on_disk = self
            .studio
            .store
            .work_dir(id)
            .join("renders")
            .join(format!("{clip}.mp4"));
        on_disk.is_file().then_some(on_disk)
    }

    /// Upload the rendered clip to YouTube (private unless told otherwise) using the clip's
    /// post copy for `youtube_shorts` (or its title).
    pub async fn publish_youtube(
        &self,
        id: &str,
        clip_id: &str,
        privacy: crate::publish::Privacy,
    ) -> Result<crate::publish::Published> {
        use crate::publish::Publisher as _;
        let creds = self.backends.youtube().ok_or_else(|| {
            StudioError::Invalid(
                "YouTube is not set up (Studio settings: client id, secret, refresh token)".into(),
            )
        })?;
        let video = self.render_path(id, clip_id).await.ok_or_else(|| {
            StudioError::NotFound("rendered video (render the clip first)".into())
        })?;
        let snap = self.open(id).await?.snapshot().await;
        let clip = snap.clip(clip_id)?;
        let post = clip
            .social
            .as_ref()
            .and_then(|s| s.posts.get("youtube_shorts").cloned())
            .unwrap_or_else(|| crate::social::PlatformPost {
                title: clip.title.clone(),
                description: clip.hook.clone().unwrap_or_default(),
                hashtags: Vec::new(),
            });
        crate::publish::Youtube::new(creds)
            .publish(&video, &post, privacy)
            .await
    }

    /// Copy the render and write captions + post text next to it.
    pub async fn export_package(&self, id: &str, clip_id: &str, dir: &Path) -> Result<PathBuf> {
        let video = self.render_path(id, clip_id).await.ok_or_else(|| {
            StudioError::NotFound("rendered video (render the clip first)".into())
        })?;
        let snap = self.open(id).await?.snapshot().await;
        let clip = snap.clip(clip_id)?;
        let name: String = clip
            .title
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == ' ' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let out = dir.join(name.trim());
        tokio::fs::create_dir_all(&out).await?;
        tokio::fs::copy(&video, out.join("clip.mp4")).await?;
        if let (Some(tr), Some(style)) = (snap.transcript.as_ref(), clip.captions.as_ref()) {
            let map = OutputMap::new(kept_ranges(clip, tr));
            let words = words_for_clip(tr, clip, &map);
            tokio::fs::write(
                out.join("captions.srt"),
                crate::captions::to_srt(&words, style),
            )
            .await?;
            tokio::fs::write(
                out.join("captions.vtt"),
                crate::captions::to_vtt(&words, style),
            )
            .await?;
        }
        if let Some(pack) = &clip.social {
            for pl in Platform::ALL {
                if let Some(t) = pack.render(pl) {
                    tokio::fs::write(out.join(format!("{}.txt", pl.key())), t).await?;
                }
            }
        }
        Ok(out)
    }
}
