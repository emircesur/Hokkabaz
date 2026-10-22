//! External tools Studio needs, installable from inside the app.
//!
//! * **ffmpeg / ffprobe**: static builds (BtbN on Windows/Linux, evermeet on macOS) downloaded into
//!   `<data>/tools/ffmpeg`, SHA-256 checked when the host publishes sums, found by every later spawn
//!   through `hk_media::resolve_program`, so no restart is needed.
//! * **Concat**: the editor's own release from GitHub (AppImage on Linux, installer on Windows,
//!   `.app` copied to `~/Applications` on macOS), located by `hk-concat` afterwards.
//!
//! Nothing here runs on Android (no executables can be downloaded and run there): the status says
//! so and points at an instance.

mod archive;
mod concat;
mod download;
mod ffmpeg;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tokio_util::sync::CancellationToken;

pub use concat::{Asset, pick_concat_asset};
pub use ffmpeg::{FfmpegBuild, ffmpeg_builds};

#[derive(Debug, thiserror::Error)]
pub enum DepsError {
    #[error("{0}")]
    Unsupported(String),
    #[error("unknown tool `{0}`")]
    Unknown(String),
    #[error("download: {0}")]
    Download(String),
    #[error("checksum mismatch for {0}: the download is corrupt or was tampered with")]
    Checksum(String),
    #[error("unpack: {0}")]
    Unpack(String),
    #[error("cancelled")]
    Cancelled,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DepsError>;

#[derive(Debug, Clone, Serialize)]
pub struct DepStatus {
    pub id: &'static str,
    pub name: &'static str,
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    /// `managed` (downloaded by the app), `system` (PATH / env / package manager).
    pub source: Option<&'static str>,
    pub can_install: bool,
    /// What the tool is for / what to do when it cannot be installed here.
    pub note: String,
    pub installing: bool,
    pub phase: Option<String>,
    pub progress: Option<(u64, u64)>,
    pub error: Option<String>,
}

#[derive(Default)]
struct Job {
    cancel: CancellationToken,
    phase: String,
    done: u64,
    total: u64,
}

#[derive(Default)]
struct State {
    active: HashMap<&'static str, Job>,
    failed: HashMap<&'static str, String>,
    /// Installed but needs a manual step (Windows installer window), shown as the note.
    notice: HashMap<&'static str, String>,
}

/// Cheap to clone; clones share the install jobs.
#[derive(Clone)]
pub struct Deps {
    tools_dir: PathBuf,
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for Deps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Deps")
            .field("tools_dir", &self.tools_dir)
            .finish_non_exhaustive()
    }
}

pub const FFMPEG: &str = "ffmpeg";
pub const CONCAT: &str = "concat";

impl Deps {
    /// Tools live under `<data_dir>/tools`. Also points `hk-media` at the managed ffmpeg folder.
    pub fn new(data_dir: &Path) -> Self {
        let tools_dir = data_dir.join("tools");
        hk_media::set_managed_dir(Some(tools_dir.join("ffmpeg")));
        Self {
            tools_dir,
            state: Arc::default(),
        }
    }

    pub fn tools_dir(&self) -> &Path {
        &self.tools_dir
    }

    /// The Concat executable to drive: one the app installed, else whatever `hk-concat` finds.
    pub fn concat_path(&self) -> Option<PathBuf> {
        concat::find(&self.tools_dir)
    }

    pub async fn status(&self) -> Vec<DepStatus> {
        vec![ffmpeg::status(self).await, concat::status(self).await]
    }

    /// Start downloading `id` in the background; poll [`Deps::status`] for progress.
    pub fn install(&self, id: &str) -> Result<()> {
        let id: &'static str = match id {
            FFMPEG => FFMPEG,
            CONCAT => CONCAT,
            other => return Err(DepsError::Unknown(other.to_owned())),
        };
        if cfg!(target_os = "android") {
            return Err(DepsError::Unsupported(
                "this device cannot install tools; connect to an instance that has them".into(),
            ));
        }
        let cancel = CancellationToken::new();
        {
            let mut s = self.lock();
            if s.active.contains_key(id) {
                return Ok(());
            }
            s.failed.remove(id);
            s.notice.remove(id);
            s.active.insert(
                id,
                Job {
                    cancel: cancel.clone(),
                    phase: "starting".into(),
                    ..Job::default()
                },
            );
        }
        let me = self.clone();
        tokio::spawn(async move {
            let r = match id {
                FFMPEG => ffmpeg::install(&me, &cancel).await,
                _ => concat::install(&me, &cancel).await,
            };
            let mut s = me.lock();
            s.active.remove(id);
            match r {
                Ok(note) => {
                    if let Some(n) = note {
                        s.notice.insert(id, n);
                    }
                }
                Err(DepsError::Cancelled) => {}
                Err(e) => {
                    tracing::warn!(tool = id, error = %e, "install failed");
                    s.failed.insert(id, e.to_string());
                }
            }
        });
        Ok(())
    }

    pub fn cancel(&self, id: &str) {
        if let Some(j) = self.lock().active.get(id) {
            j.cancel.cancel();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn set_phase(&self, id: &'static str, phase: &str) {
        if let Some(j) = self.lock().active.get_mut(id) {
            j.phase = phase.to_owned();
            j.done = 0;
            j.total = 0;
        }
    }

    pub(crate) fn set_progress(&self, id: &'static str, done: u64, total: u64) {
        if let Some(j) = self.lock().active.get_mut(id) {
            j.done = done;
            j.total = total;
        }
    }

    /// Job state for the status rows.
    pub(crate) fn job_view(&self, id: &'static str) -> JobView {
        let s = self.lock();
        match s.active.get(id) {
            Some(j) => JobView {
                installing: true,
                phase: Some(j.phase.clone()),
                progress: (j.total > 0).then_some((j.done, j.total)),
                ..JobView::default()
            },
            None => JobView {
                error: s.failed.get(id).cloned(),
                notice: s.notice.get(id).cloned(),
                ..JobView::default()
            },
        }
    }
}

#[derive(Default)]
pub(crate) struct JobView {
    pub installing: bool,
    pub phase: Option<String>,
    pub progress: Option<(u64, u64)>,
    pub error: Option<String>,
    pub notice: Option<String>,
}

/// First line of `<bin> <arg>` output, if it runs.
pub(crate) async fn run_version(bin: &Path, arg: &str) -> Option<String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new(bin)
            .arg(arg)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned()
    })
}
