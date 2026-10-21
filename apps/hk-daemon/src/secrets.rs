//! Secret backend for headless hosts: a 0600 JSON file in the data dir, used when the OS
//! keyring is unavailable (no Secret Service on a server). Values stay out of Debug output.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Mutex;

use hk_store::{Secret, SecretError, SecretStore};

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

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_mode() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileSecretStore::new(dir.path().join("secrets.json"));
        assert!(s.get("a").unwrap().is_none());
        s.set("a", &Secret::new("v1")).unwrap();
        assert_eq!(s.get("a").unwrap().unwrap().expose(), "v1");
        s.delete("a").unwrap();
        assert!(s.get("a").unwrap().is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("secrets.json")).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0);
        }
    }
}
