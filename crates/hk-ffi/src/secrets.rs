//! Secret backend selection.
//!
//! Desktop prefers the OS keyring. Where it is unusable (headless Linux without Secret Service)
//! keys live in a 0600 JSON file inside the app-private data dir. On mobile the Dart side first
//! hands over a master key kept in the Android Keystore / iOS Keychain ([`set_master_key`]); the
//! file is then ChaCha20-Poly1305 encrypted with it, so the key never touches disk next to the
//! data. Values stay out of Debug output.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use hk_store::{Secret, SecretError, SecretStore};

use crate::dto::CoreProfile;

pub struct FileSecretStore {
    path: PathBuf,
    lock: Mutex<()>,
    key: Option<[u8; 32]>,
}

/// File magic of the encrypted format (`HKS1` + 12-byte nonce + ciphertext+tag).
const MAGIC: &[u8; 4] = b"HKS1";

static MASTER_KEY: RwLock<Option<[u8; 32]>> = RwLock::new(None);

/// Master key for encrypting the secrets file (from the platform keystore). Call before
/// `core_init`. Exactly 32 bytes.
pub fn set_master_key(key: &[u8]) -> Result<(), SecretError> {
    let k: [u8; 32] = key.try_into().map_err(|_| be("master key must be 32 bytes"))?;
    *MASTER_KEY.write().unwrap_or_else(PoisonError::into_inner) = Some(k);
    Ok(())
}

fn master_key() -> Option<[u8; 32]> {
    *MASTER_KEY.read().unwrap_or_else(PoisonError::into_inner)
}

fn seal(key: &[u8; 32], plain: &[u8]) -> Result<Vec<u8>, SecretError> {
    use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
    use ring::rand::{SecureRandom, SystemRandom};
    let k = LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).map_err(|_| be("bad key"))?);
    let mut nonce = [0u8; 12];
    SystemRandom::new().fill(&mut nonce).map_err(|_| be("rng failed"))?;
    let mut buf = plain.to_vec();
    k.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(MAGIC), &mut buf).map_err(|_| be("encrypt failed"))?;
    let mut out = Vec::with_capacity(4 + 12 + buf.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&buf);
    Ok(out)
}

fn open(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>, SecretError> {
    use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
    if data.len() < 4 + 12 + 16 || &data[..4] != MAGIC {
        return Err(be("secrets file is not in the encrypted format"));
    }
    let k = LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).map_err(|_| be("bad key"))?);
    let nonce = Nonce::try_assume_unique_for_key(&data[4..16]).map_err(|_| be("bad nonce"))?;
    let mut buf = data[16..].to_vec();
    let plain = k.open_in_place(nonce, Aad::from(MAGIC), &mut buf).map_err(|_| be("cannot decrypt secrets (wrong key or damaged file)"))?;
    Ok(plain.to_vec())
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
        Self { path: path.into(), lock: Mutex::new(()), key: None }
    }

    /// Encrypt the file with `key`. A plaintext file from before the key existed is read and
    /// re-written encrypted on the next change.
    pub fn encrypted(path: impl Into<PathBuf>, key: [u8; 32]) -> Self {
        Self { path: path.into(), lock: Mutex::new(()), key: Some(key) }
    }

    fn load(&self) -> Result<BTreeMap<String, String>, SecretError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(be(e)),
        };
        let plain = if bytes.starts_with(MAGIC) {
            let key = self.key.as_ref().ok_or_else(|| be("secrets file is encrypted but no master key is available"))?;
            open(key, &bytes)?
        } else {
            bytes // legacy plaintext JSON
        };
        serde_json::from_slice(&plain).map_err(be)
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
        let json = serde_json::to_vec(map).map_err(be)?;
        let bytes = match &self.key {
            Some(k) => seal(k, &json)?,
            None => json,
        };
        // Write-then-rename so a crash never leaves a half-written (undecryptable) file.
        let tmp = self.path.with_extension("tmp");
        let mut f = opts.open(&tmp).map_err(be)?;
        f.write_all(&bytes).map_err(be)?;
        f.sync_all().map_err(be)?;
        std::fs::rename(&tmp, &self.path).map_err(be)
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
    let path = data_dir.join("secrets.json");
    match master_key() {
        Some(k) => Arc::new(FileSecretStore::encrypted(path, k)),
        None => Arc::new(FileSecretStore::new(path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hk_sec_{name}_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("secrets.json")
    }

    #[test]
    fn encrypted_roundtrip_hides_values() {
        let p = tmp("rt");
        let s = FileSecretStore::encrypted(&p, [7; 32]);
        s.set("a", &Secret::new("sk-super-secret")).unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert!(raw.starts_with(MAGIC));
        assert!(!String::from_utf8_lossy(&raw).contains("sk-super-secret"));
        assert_eq!(s.get("a").unwrap().unwrap().expose(), "sk-super-secret");
        // A second store with the same key reads it; another key cannot.
        assert!(FileSecretStore::encrypted(&p, [7; 32]).get("a").unwrap().is_some());
        assert!(FileSecretStore::encrypted(&p, [8; 32]).get("a").is_err());
        assert!(FileSecretStore::new(&p).get("a").is_err(), "no key, encrypted file");
        s.delete("a").unwrap();
        assert!(s.get("a").unwrap().is_none());
    }

    #[test]
    fn plaintext_file_migrates_on_next_write() {
        let p = tmp("mig");
        FileSecretStore::new(&p).set("old", &Secret::new("v1")).unwrap();
        assert!(std::fs::read(&p).unwrap().starts_with(b"{"));
        let s = FileSecretStore::encrypted(&p, [1; 32]);
        assert_eq!(s.get("old").unwrap().unwrap().expose(), "v1");
        s.set("new", &Secret::new("v2")).unwrap();
        assert!(std::fs::read(&p).unwrap().starts_with(MAGIC));
        assert_eq!(s.get("old").unwrap().unwrap().expose(), "v1");
        assert_eq!(s.get("new").unwrap().unwrap().expose(), "v2");
    }

    #[test]
    fn tampering_is_detected() {
        let p = tmp("tamper");
        let s = FileSecretStore::encrypted(&p, [3; 32]);
        s.set("a", &Secret::new("x")).unwrap();
        let mut raw = std::fs::read(&p).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        std::fs::write(&p, raw).unwrap();
        assert!(s.get("a").is_err());
    }

    #[test]
    fn master_key_must_be_32_bytes() {
        assert!(set_master_key(&[0; 31]).is_err());
    }
}
