//! Project model (EDL) and its on-disk store.
//!
//! A project has one primary source video with a word-level transcript; each
//! [`Clip`] is a plan over that source (range, deleted words, captions,
//! reframing, B-roll, music). Nothing here touches ffmpeg.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hk_stt::Transcript;
use serde::{Deserialize, Serialize};

use crate::broll::BrollPlacement;
use crate::captions::CaptionStyle;
use crate::error::{Result, StudioError};
use crate::highlights::ViralScore;
use crate::reframe::ReframeSpec;
use crate::social::SocialPack;

/// The imported video.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRef {
    pub path: PathBuf,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_audio: bool,
}

/// Remove pauses longer than `min_gap`, leaving `keep` seconds on each side.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SilenceCut {
    pub min_gap: f64,
    pub keep: f64,
}

impl Default for SilenceCut {
    fn default() -> Self {
        Self { min_gap: 0.6, keep: 0.12 }
    }
}

/// Background music under the clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MusicSpec {
    pub path: PathBuf,
    pub gain: f64,
    pub duck: bool,
}

/// One short clip cut from the source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: String,
    pub title: String,
    /// Source seconds.
    pub start: f64,
    pub end: f64,
    #[serde(default)]
    pub hook: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// Transcript word indices removed by text editing.
    #[serde(default)]
    pub deleted_words: BTreeSet<usize>,
    #[serde(default)]
    pub remove_fillers: bool,
    #[serde(default)]
    pub silence: Option<SilenceCut>,
    #[serde(default)]
    pub captions: Option<CaptionStyle>,
    #[serde(default)]
    pub reframe: Option<ReframeSpec>,
    #[serde(default)]
    pub broll: Vec<BrollPlacement>,
    #[serde(default)]
    pub music: Option<MusicSpec>,
    #[serde(default)]
    pub score: Option<ViralScore>,
    #[serde(default)]
    pub social: Option<SocialPack>,
    /// Template this clip was styled from.
    #[serde(default)]
    pub template: Option<String>,
}

impl Clip {
    pub fn new(id: impl Into<String>, title: impl Into<String>, start: f64, end: f64) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            start,
            end,
            hook: None,
            reason: None,
            deleted_words: BTreeSet::new(),
            remove_fillers: false,
            silence: None,
            captions: None,
            reframe: None,
            broll: Vec::new(),
            music: None,
            score: None,
            social: None,
            template: None,
        }
    }

    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

/// A Studio project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudioProject {
    pub id: String,
    pub name: String,
    pub created_ms: u64,
    pub source: SourceRef,
    #[serde(default)]
    pub transcript: Option<Transcript>,
    #[serde(default)]
    pub clips: Vec<Clip>,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Short unique-enough id (time + process counter); not a security token.
pub fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!("{prefix}{:x}{:x}", now_ms(), N.fetch_add(1, Ordering::Relaxed))
}

impl StudioProject {
    pub fn new(name: impl Into<String>, source: SourceRef) -> Self {
        Self {
            id: new_id("p"),
            name: name.into(),
            created_ms: now_ms(),
            source,
            transcript: None,
            clips: Vec::new(),
        }
    }

    pub fn clip(&self, id: &str) -> Result<&Clip> {
        self.clips.iter().find(|c| c.id == id).ok_or_else(|| StudioError::NotFound(format!("clip {id}")))
    }

    pub fn clip_mut(&mut self, id: &str) -> Result<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id).ok_or_else(|| StudioError::NotFound(format!("clip {id}")))
    }

    pub fn transcript(&self) -> Result<&Transcript> {
        self.transcript.as_ref().ok_or_else(|| StudioError::Invalid("project has no transcript yet".into()))
    }

    /// Add a clip, clamping it to the source.
    pub fn add_clip(&mut self, title: impl Into<String>, start: f64, end: f64) -> Result<&mut Clip> {
        let (s, e) = (start.max(0.0), end.min(self.source.duration));
        if !(e - s > 0.1) {
            return Err(StudioError::Invalid(format!("empty clip range {start}..{end}")));
        }
        self.clips.push(Clip::new(new_id("c"), title, s, e));
        Ok(self.clips.last_mut().expect("just pushed"))
    }

    /// Toggle deletion of transcript words (text-based editing).
    pub fn set_words_deleted(&mut self, clip_id: &str, words: impl IntoIterator<Item = usize>, deleted: bool) -> Result<()> {
        let n = self.transcript()?.words.len();
        let clip = self.clip_mut(clip_id)?;
        for w in words {
            if w >= n {
                return Err(StudioError::Invalid(format!("word index {w} out of range")));
            }
            if deleted {
                clip.deleted_words.insert(w);
            } else {
                clip.deleted_words.remove(&w);
            }
        }
        Ok(())
    }
}

/// Projects as `<root>/<id>/project.json`.
#[derive(Debug, Clone)]
pub struct ProjectStore {
    root: PathBuf,
}

impl ProjectStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn check_id(id: &str) -> Result<()> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(StudioError::Invalid(format!("bad project id {id:?}")));
        }
        Ok(())
    }

    pub async fn save(&self, p: &StudioProject) -> Result<()> {
        Self::check_id(&p.id)?;
        let dir = self.dir(&p.id);
        tokio::fs::create_dir_all(&dir).await?;
        let tmp = dir.join("project.json.tmp");
        tokio::fs::write(&tmp, serde_json::to_vec_pretty(p)?).await?;
        tokio::fs::rename(&tmp, dir.join("project.json")).await?;
        Ok(())
    }

    pub async fn load(&self, id: &str) -> Result<StudioProject> {
        Self::check_id(id)?;
        let bytes = tokio::fs::read(self.dir(id).join("project.json"))
            .await
            .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { StudioError::NotFound(format!("project {id}")) } else { e.into() })?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub async fn list(&self) -> Result<Vec<StudioProject>> {
        let mut out = Vec::new();
        let mut rd = match tokio::fs::read_dir(&self.root).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        while let Some(ent) = rd.next_entry().await? {
            if let Some(id) = ent.file_name().to_str() {
                if let Ok(p) = self.load(id).await {
                    out.push(p);
                }
            }
        }
        out.sort_by_key(|p| std::cmp::Reverse(p.created_ms));
        Ok(out)
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        Self::check_id(id)?;
        match tokio::fs::remove_dir_all(self.dir(id)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Directory for renders / proxies / thumbs of one project.
    pub fn work_dir(&self, id: &str) -> PathBuf {
        self.dir(id)
    }
}

/// Probe `path` into a [`SourceRef`].
pub async fn probe_source(path: &Path) -> Result<SourceRef> {
    let info = hk_media::probe(path).await?;
    let (w, h) = info.display_size().ok_or_else(|| StudioError::Invalid("source has no video stream".into()))?;
    Ok(SourceRef {
        path: path.to_path_buf(),
        duration: info.duration.unwrap_or(0.0),
        width: w,
        height: h,
        fps: info.video().and_then(|v| v.fps).unwrap_or(30.0),
        has_audio: info.has_audio(),
    })
}
