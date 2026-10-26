//! Models on disk: `<dir>/<file>.gguf` for catalog models, `custom.json` for GGUF files the
//! user points at. Downloads resume from `<file>.part` and are verified against the catalog's
//! SHA-256 before they appear as installed.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::catalog::{self, CatalogEntry};
use crate::error::{LocalError, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstalledModel {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    /// Added by path rather than downloaded from the catalog.
    pub custom: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Custom {
    id: String,
    name: String,
    path: PathBuf,
}

#[derive(Clone)]
pub struct ModelStore {
    dir: PathBuf,
    /// Downloadable entries this store knows (LLMs by default; see [`ModelStore::with_catalog`]).
    catalog: Arc<Vec<CatalogEntry>>,
    client: reqwest::Client,
    active: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for ModelStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelStore")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

fn slug(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    out.truncate(60);
    out.trim_matches('-').to_owned()
}

impl ModelStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self::with_catalog(dir, catalog::builtin())
    }

    /// Store over another catalog, e.g. the whisper speech models.
    pub fn with_catalog(dir: impl Into<PathBuf>, catalog: Vec<CatalogEntry>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Self {
            dir: dir.into(),
            catalog: Arc::new(catalog),
            client,
            active: Arc::default(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn custom_path(&self) -> PathBuf {
        self.dir.join("custom.json")
    }

    fn read_custom(&self) -> Vec<Custom> {
        std::fs::read(self.custom_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write_custom(&self, v: &[Custom]) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(
            self.custom_path(),
            serde_json::to_vec_pretty(v).map_err(|e| LocalError::Invalid(e.to_string()))?,
        )?;
        Ok(())
    }

    pub fn installed(&self) -> Vec<InstalledModel> {
        let mut out = Vec::new();
        for m in self.catalog.iter().cloned() {
            let p = self.dir.join(&m.file);
            if let Ok(md) = std::fs::metadata(&p)
                && md.len() == m.size_bytes
            {
                out.push(InstalledModel {
                    id: m.id,
                    name: m.name,
                    path: p,
                    size_bytes: md.len(),
                    custom: false,
                });
            }
        }
        for c in self.read_custom() {
            if let Ok(md) = std::fs::metadata(&c.path) {
                out.push(InstalledModel {
                    id: c.id,
                    name: c.name,
                    path: c.path,
                    size_bytes: md.len(),
                    custom: true,
                });
            }
        }
        out
    }

    pub fn get(&self, id: &str) -> Option<InstalledModel> {
        self.installed().into_iter().find(|m| m.id == id)
    }

    /// Ids with a download in flight.
    pub fn downloading(&self) -> Vec<String> {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Register a GGUF file already on disk (it is used in place, not copied).
    pub fn add_custom(&self, path: &Path, name: Option<&str>) -> Result<InstalledModel> {
        let md = std::fs::metadata(path)?;
        if !md.is_file()
            || path
                .extension()
                .and_then(|e| e.to_str())
                .is_none_or(|e| !e.eq_ignore_ascii_case("gguf"))
        {
            return Err(LocalError::Invalid("choose a .gguf file".into()));
        }
        let mut head = [0u8; 4];
        std::fs::File::open(path)?.read_exact_compat(&mut head)?;
        if &head != b"GGUF" {
            return Err(LocalError::Invalid("not a GGUF file".into()));
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model");
        let id = format!("custom-{}", slug(stem));
        let name = name.map(str::to_owned).unwrap_or_else(|| stem.to_owned());
        let mut all = self.read_custom();
        all.retain(|c| c.id != id);
        all.push(Custom {
            id: id.clone(),
            name: name.clone(),
            path: path.to_owned(),
        });
        self.write_custom(&all)?;
        Ok(InstalledModel {
            id,
            name,
            path: path.to_owned(),
            size_bytes: md.len(),
            custom: true,
        })
    }

    /// Delete a downloaded model, or forget a custom one (its file is left alone).
    pub fn remove(&self, id: &str) -> Result<()> {
        if let Some(m) = catalog::builtin().into_iter().find(|m| m.id == id) {
            let _ = std::fs::remove_file(self.dir.join(format!("{}.part", m.file)));
            match std::fs::remove_file(self.dir.join(&m.file)) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
        let mut all = self.read_custom();
        let before = all.len();
        all.retain(|c| c.id != id);
        if all.len() == before {
            return Err(LocalError::UnknownModel(id.to_owned()));
        }
        self.write_custom(&all)
    }

    /// Download `entry`, resuming a partial file. `progress` is called at most a few times a second.
    pub async fn download(
        &self,
        entry: &CatalogEntry,
        progress: impl Fn(DownloadProgress) + Send + Sync,
        cancel: CancellationToken,
    ) -> Result<InstalledModel> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let final_path = self.dir.join(&entry.file);
        if self.get(&entry.id).is_some() {
            return Ok(self.get(&entry.id).expect("checked"));
        }
        {
            let mut a = self.active.lock().unwrap_or_else(PoisonError::into_inner);
            if a.contains(&entry.id) {
                return Err(LocalError::Invalid(format!(
                    "{} is already downloading",
                    entry.id
                )));
            }
            a.push(entry.id.clone());
        }
        let result = self
            .download_inner(entry, &final_path, &progress, &cancel)
            .await;
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|i| i != &entry.id);
        result?;
        Ok(InstalledModel {
            id: entry.id.clone(),
            name: entry.name.clone(),
            path: final_path,
            size_bytes: entry.size_bytes,
            custom: false,
        })
    }

    async fn download_inner(
        &self,
        entry: &CatalogEntry,
        final_path: &Path,
        progress: &(impl Fn(DownloadProgress) + Send + Sync),
        cancel: &CancellationToken,
    ) -> Result<()> {
        let part = self.dir.join(format!("{}.part", entry.file));
        let mut have = tokio::fs::metadata(&part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        if have > entry.size_bytes {
            tokio::fs::remove_file(&part).await?;
            have = 0;
        }
        if have < entry.size_bytes {
            let mut req = self.client.get(&entry.url);
            if have > 0 {
                req = req.header("Range", format!("bytes={have}-"));
            }
            let resp = req.send().await.map_err(|e| {
                LocalError::Download(format!("cannot reach the model host: {}", e.without_url()))
            })?;
            let status = resp.status();
            if !(status.is_success()) {
                return Err(LocalError::Download(format!(
                    "the model host answered {status}"
                )));
            }
            // A server that ignores Range sends the whole file again.
            let resumed = status.as_u16() == 206;
            if !resumed {
                have = 0;
            }
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(resumed)
                .truncate(!resumed)
                .open(&part)
                .await?;
            let mut done = have;
            let mut last = std::time::Instant::now();
            let mut stream = resp.bytes_stream();
            loop {
                let next = tokio::select! {
                    () = cancel.cancelled() => { file.flush().await?; return Err(LocalError::Cancelled); }
                    n = stream.next() => n,
                };
                let Some(chunk) = next else { break };
                let chunk = chunk.map_err(|e| {
                    LocalError::Download(format!(
                        "connection lost ({}); run it again to resume",
                        e.without_url()
                    ))
                })?;
                file.write_all(&chunk).await?;
                done += chunk.len() as u64;
                if last.elapsed() >= std::time::Duration::from_millis(250) {
                    last = std::time::Instant::now();
                    progress(DownloadProgress {
                        done,
                        total: entry.size_bytes,
                    });
                }
            }
            file.flush().await?;
            progress(DownloadProgress {
                done,
                total: entry.size_bytes,
            });
            if done != entry.size_bytes {
                return Err(LocalError::Download(format!(
                    "incomplete download ({done} of {} bytes); run it again to resume",
                    entry.size_bytes
                )));
            }
        }
        let actual = sha256_file(&part).await?;
        if !actual.eq_ignore_ascii_case(&entry.sha256) {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(LocalError::Checksum(entry.id.clone()));
        }
        let (part2, fin) = (part.clone(), final_path.to_path_buf());
        tokio::task::spawn_blocking(move || crate::replace::replace_file(&part2, &fin))
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))??;
        Ok(())
    }
}

trait ReadExactCompat {
    fn read_exact_compat(&mut self, buf: &mut [u8]) -> std::io::Result<()>;
}

impl ReadExactCompat for std::fs::File {
    fn read_exact_compat(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        std::io::Read::read_exact(self, buf)
    }
}

async fn sha256_file(path: &Path) -> Result<String> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn serve(body: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                    let start = req
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes="))
                        .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok());
                    let (status, slice) = match start {
                        Some(s0) => ("206 Partial Content", &body[s0..]),
                        None => ("200 OK", &body[..]),
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        slice.len()
                    );
                    let _ = s.write_all(head.as_bytes()).await;
                    let _ = s.write_all(slice).await;
                    let _ = s.shutdown().await;
                });
            }
        });
        format!("http://{addr}/m.gguf")
    }

    fn entry(url: String, body: &[u8]) -> CatalogEntry {
        let mut e = catalog::builtin().remove(0);
        e.url = url;
        e.size_bytes = body.len() as u64;
        e.sha256 = hex::encode(Sha256::digest(body));
        e
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hk-local-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[tokio::test]
    async fn downloads_verifies_and_resumes() {
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let url = serve(body.clone()).await;
        let e = entry(url, &body);
        let dir = tmp("dl");
        let store = ModelStore::new(&dir);
        // A half-finished earlier attempt.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{}.part", e.file)), &body[..70_000]).unwrap();
        let m = store
            .download(&e, |_| {}, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&m.path).unwrap(), body);
        // `installed()` checks the catalog's real size, which this fixture does not have.
        store.remove(&e.id).unwrap();
        assert!(!m.path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn bad_checksum_discards_the_file() {
        let body = vec![7u8; 5000];
        let url = serve(body.clone()).await;
        let mut e = entry(url, &body);
        e.sha256 = "0".repeat(64);
        let dir = tmp("bad");
        let store = ModelStore::new(&dir);
        let err = store
            .download(&e, |_| {}, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, LocalError::Checksum(_)), "{err}");
        assert!(store.installed().is_empty());
        assert!(!dir.join(format!("{}.part", e.file)).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn custom_models_need_a_gguf_header() {
        let dir = tmp("custom");
        std::fs::create_dir_all(&dir).unwrap();
        let store = ModelStore::new(dir.join("models"));
        let bad = dir.join("x.gguf");
        std::fs::write(&bad, b"nope").unwrap();
        assert!(store.add_custom(&bad, None).is_err());
        let good = dir.join("My Model.gguf");
        std::fs::write(&good, b"GGUF....").unwrap();
        let m = store.add_custom(&good, None).unwrap();
        assert_eq!(m.id, "custom-my-model");
        assert_eq!(store.installed().len(), 1);
        store.remove(&m.id).unwrap();
        assert!(store.installed().is_empty() && good.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
