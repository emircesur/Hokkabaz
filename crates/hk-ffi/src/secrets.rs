//! Secret backend selection.
//!
//! Desktop prefers the OS keyring. Where it is unusable (headless Linux without Secret Service)
//! and on mobile (Keystore bridge lands in P6.2) keys live in a 0600 JSON file inside the
//! app-private data dir. Values stay out of Debug output.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hk_store::{Secret, SecretError, SecretStore};

use crate::dto::CoreProfile;

pub struct FileSecretStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl fmt::Debug for FileSecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSecretStore").field("path", &self.path).finish()
    }
}

fn be(e: impl fmt::Display) -> SecretError {
    SecretError::Backend(e.to_string())
}

impl FileSecretStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: Mutex::new(()) }
    }

    fn load(&self) -> Result<BTreeMap<String, String>, SecretError> {
        match std::fs::read(&self.path) {
            Ok(b) => serde_json::from_slice(&b).map_err(be),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(be(e)),
        }
    }

    fn save(&self, map: &BTreeMap<String, String>) -> Result<(), SecretError> {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&self.path).map_err(be)?;
        f.write_all(&serde_json::to_vec(map).map_err(be)?).map_err(be)
    }

    fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SecretStore for FileSecretStore {
    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        let _g = self.guard();
        Ok(self.load()?.get(name).map(|v| Secret::new(v.clone())))
    }

    fn set(&self, name: &str, value: &Secret) -> Result<(), SecretError> {
        let _g = self.guard();
        let mut m = self.load()?;
        m.insert(name.to_owned(), value.expose().to_owned());
        self.save(&m)
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        let _g = self.guard();
        let mut m = self.load()?;
        if m.remove(name).is_some() {
            self.save(&m)?;
        }
        Ok(())
    }
}

/// Keyring on desktop when it answers, else the file store.
pub fn select(profile: CoreProfile, data_dir: &std::path::Path) -> Arc<dyn SecretStore> {
    #[cfg(not(target_os = "android"))]
    if profile == CoreProfile::Desktop {
        let kr = hk_store::secrets::KeyringSecretStore::default();
        // Read-only probe: an error means no usable backend, not a missing key.
        match kr.get("__hk_probe") {
            Ok(_) => return Arc::new(kr),
            Err(e) => tracing::warn!(error = %e, "OS keyring unavailable; using secrets file"),
        }
    }
    #[cfg(target_os = "android")]
    let _ = profile;
    Arc::new(FileSecretStore::new(data_dir.join("secrets.json")))
}
