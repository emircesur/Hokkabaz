//! Speech models for local transcription: what is installed under `<data>/whisper`, background
//! downloads with progress, and the path the transcriber loads. Transcription itself runs in the
//! `hk-whisper` helper process (see `hk_stt::external`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use hk_local::{CatalogEntry, DownloadProgress, ModelStore, whisper};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{Result, StudioError};

/// Model used when the settings name none.
pub const DEFAULT_MODEL: &str = "base";

/// Where `id` lives (whether or not it is downloaded yet).
pub fn model_path(data_dir: &Path, id: &str) -> Option<PathBuf> {
    whisper::catalog()
        .into_iter()
        .find(|e| e.id == id)
        .map(|e| data_dir.join("whisper").join(e.file))
}

#[derive(Default)]
struct Jobs {
    active: HashMap<String, (CancellationToken, DownloadProgress)>,
    failed: HashMap<String, String>,
}

pub struct WhisperModels {
    store: ModelStore,
    jobs: Arc<Mutex<Jobs>>,
}

impl std::fmt::Debug for WhisperModels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WhisperModels")
    }
}

impl WhisperModels {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            store: ModelStore::with_catalog(data_dir.join("whisper"), whisper::catalog()),
            jobs: Arc::default(),
        }
    }

    fn entry(id: &str) -> Result<CatalogEntry> {
        whisper::catalog()
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| StudioError::NotFound(format!("no speech model `{id}`")))
    }

    /// Catalog with install / download state, plus whether this build can run them at all.
    pub fn status(&self) -> Value {
        let have = self.store.installed();
        let jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let models: Vec<Value> = whisper::catalog()
            .into_iter()
            .map(|e| {
                let job = jobs.active.get(&e.id).map(|(_, p)| *p);
                json!({
                    "id": e.id, "name": e.name, "size_bytes": e.size_bytes, "note": e.note,
                    "installed": have.iter().any(|m| m.id == e.id),
                    "downloading": job.is_some(),
                    "progress": job.map(|p| json!({"done": p.done, "total": p.total})),
                    "error": jobs.failed.get(&e.id),
                })
            })
            .collect();
        json!({
            "helper": hk_stt::external::find_helper().is_some(),
            "gpu": hk_stt::external::helper_gpu(),
            "models": models,
        })
    }

    /// Start a background download (resumes a partial file). No-op when installed.
    pub fn pull(&self, id: &str) -> Result<()> {
        let entry = Self::entry(id)?;
        if self.store.get(id).is_some() {
            return Ok(());
        }
        let cancel = CancellationToken::new();
        {
            let mut j = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            if j.active.contains_key(id) {
                return Ok(());
            }
            j.failed.remove(id);
            j.active.insert(
                id.to_owned(),
                (
                    cancel.clone(),
                    DownloadProgress {
                        done: 0,
                        total: entry.size_bytes,
                    },
                ),
            );
        }
        let store = self.store.clone();
        let jobs = self.jobs.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let track = jobs.clone();
            let key = id.clone();
            let r = store
                .download(
                    &entry,
                    move |p| {
                        if let Some(a) = track
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .active
                            .get_mut(&key)
                        {
                            a.1 = p;
                        }
                    },
                    cancel,
                )
                .await;
            let mut j = jobs.lock().unwrap_or_else(PoisonError::into_inner);
            j.active.remove(&id);
            match r {
                Ok(_) | Err(hk_local::LocalError::Cancelled) => {}
                Err(e) => {
                    tracing::warn!(model = %id, error = %e, "speech model download failed");
                    j.failed.insert(id, e.to_string());
                }
            }
        });
        Ok(())
    }

    pub fn cancel(&self, id: &str) {
        if let Some((c, _)) = self
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .active
            .get(id)
        {
            c.cancel();
        }
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let e = Self::entry(id)?;
        self.cancel(id);
        let _ = std::fs::remove_file(self.store.dir().join(&e.file));
        let _ = std::fs::remove_file(self.store.dir().join(format!("{}.part", e.file)));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_the_catalog() {
        let p = model_path(Path::new("/d"), "base").unwrap();
        assert!(p.ends_with("whisper/ggml-base-q5_1.bin"));
        assert!(model_path(Path::new("/d"), "nope").is_none());
    }

    #[test]
    fn status_lists_every_model_uninstalled() {
        let d = tempfile::tempdir().unwrap();
        let s = WhisperModels::new(d.path()).status();
        let m = s["models"].as_array().unwrap();
        assert_eq!(m.len(), 5);
        assert!(m.iter().all(|x| x["installed"] == false));
        assert!(WhisperModels::new(d.path()).pull("zzz").is_err());
    }
}
