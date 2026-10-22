//! Instance book: "This device" plus any number of remote `hokkabazd`s, and the hot swap of
//! the active [`crate::handle::CoreHandle`] between them.
//!
//! Persisted as `<data_dir>/instances.json` (name, URL, ids only). Tokens live in the secret
//! backend under `instance/<id>/token` and never leave Rust.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use hk_store::{Secret, SecretStore};
use serde::{Deserialize, Serialize};

use crate::dto::{CoreProfile, InstanceDto, InstanceKindDto, InstanceStateDto, InstanceStatusDto};
use crate::error::{HkError, HkErrorKind};
use crate::handle;
use crate::remote::{self, RemoteCore};

pub const LOCAL_ID: &str = "local";
const LOCAL_NAME: &str = "This device (local)";
const FILE: &str = "instances.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    id: String,
    name: String,
    base_url: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Book {
    /// Connected at app start; `None` = local.
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    instances: Vec<Stored>,
}

struct State {
    book: Book,
    /// Id of the core `handle::core()` points at.
    connected: String,
}

pub struct Manager {
    path: PathBuf,
    profile: CoreProfile,
    secrets: Arc<dyn SecretStore>,
    state: Mutex<State>,
    /// Serialises connects so two taps cannot interleave probe and swap.
    connect_lock: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manager")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

fn token_name(id: &str) -> String {
    format!("instance/{id}/token")
}

fn not_found(id: &str) -> HkError {
    HkError::new(HkErrorKind::NotFound, format!("no instance `{id}`"))
}

impl Manager {
    /// Load the book from `dir`; an unreadable file starts empty (the local core must still run).
    pub fn open(dir: &Path, profile: CoreProfile, secrets: Arc<dyn SecretStore>) -> Self {
        let path = dir.join(FILE);
        let book = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "instances.json unreadable; starting empty");
                Book::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Book::default(),
            Err(e) => {
                tracing::warn!(error = %e, "instances.json unreadable; starting empty");
                Book::default()
            }
        };
        Self {
            path,
            profile,
            secrets,
            state: Mutex::new(State {
                book,
                connected: LOCAL_ID.to_owned(),
            }),
            connect_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn save(&self, book: &Book) -> Result<(), HkError> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(book)?)
            .and_then(|()| std::fs::rename(&tmp, &self.path))
            .map_err(|e| HkError::internal(format!("instances.json: {e}")))
    }

    fn has_token(&self, id: &str) -> bool {
        matches!(self.secrets.get(&token_name(id)), Ok(Some(_)))
    }

    fn dto(&self, s: &Stored, st: &State) -> InstanceDto {
        InstanceDto {
            id: s.id.clone(),
            name: s.name.clone(),
            base_url: Some(s.base_url.clone()),
            kind: InstanceKindDto::Remote,
            connected: st.connected == s.id,
            is_default: st.book.default.as_deref() == Some(&s.id),
            has_token: self.has_token(&s.id),
        }
    }

    fn local_dto(st: &State) -> InstanceDto {
        InstanceDto {
            id: LOCAL_ID.to_owned(),
            name: LOCAL_NAME.to_owned(),
            base_url: None,
            kind: InstanceKindDto::Local,
            connected: st.connected == LOCAL_ID,
            is_default: st.book.default.as_deref().is_none_or(|d| d == LOCAL_ID),
            has_token: false,
        }
    }

    /// The local entry first, then remotes in the order they were added.
    pub fn list(&self) -> Vec<InstanceDto> {
        let st = self.lock();
        let mut out = vec![Self::local_dto(&st)];
        out.extend(st.book.instances.iter().map(|s| self.dto(s, &st)));
        out
    }

    fn get(&self, id: &str) -> Result<InstanceDto, HkError> {
        self.list()
            .into_iter()
            .find(|i| i.id == id)
            .ok_or_else(|| not_found(id))
    }

    pub fn add(&self, name: &str, base_url: &str, token: &str) -> Result<InstanceDto, HkError> {
        let url = remote::parse_base_url(base_url)?;
        let base_url = url.as_str().trim_end_matches('/').to_owned();
        let token = token.trim();
        if token.is_empty() {
            return Err(HkError::invalid("token must not be empty"));
        }
        let name = match name.trim() {
            "" => host_label(&url),
            n => n.to_owned(),
        };
        let mut st = self.lock();
        if let Some(dup) = st.book.instances.iter().find(|i| i.base_url == base_url) {
            return Err(HkError::new(
                HkErrorKind::Conflict,
                format!("already added as `{}`", dup.name),
            ));
        }
        let id = new_id(&st.book);
        self.secrets.set(&token_name(&id), &Secret::new(token))?;
        let stored = Stored {
            id: id.clone(),
            name,
            base_url,
        };
        let mut next = Book {
            default: st.book.default.clone(),
            instances: st.book.instances.clone(),
        };
        next.instances.push(stored.clone());
        if let Err(e) = self.save(&next) {
            let _ = self.secrets.delete(&token_name(&id));
            return Err(e);
        }
        st.book = next;
        Ok(self.dto(&stored, &st))
    }

    /// Forget a remote and its token. Removing the connected one falls back to local first.
    pub async fn remove(&self, id: &str) -> Result<(), HkError> {
        if id == LOCAL_ID {
            return Err(HkError::invalid("the local instance cannot be removed"));
        }
        self.get(id)?;
        if self.lock().connected == id {
            self.connect(LOCAL_ID).await?;
        }
        let mut st = self.lock();
        let mut next = Book {
            default: st.book.default.clone(),
            instances: st.book.instances.clone(),
        };
        next.instances.retain(|i| i.id != id);
        if next.default.as_deref() == Some(id) {
            next.default = None;
        }
        self.save(&next)?;
        st.book = next;
        if let Err(e) = self.secrets.delete(&token_name(id)) {
            tracing::warn!(error = %e, "could not delete instance token");
        }
        Ok(())
    }

    /// Make `id` the instance connected at the next app start (does not switch now).
    pub fn select(&self, id: &str) -> Result<InstanceDto, HkError> {
        self.get(id)?;
        self.set_default(id)?;
        self.get(id)
    }

    fn set_default(&self, id: &str) -> Result<(), HkError> {
        let mut st = self.lock();
        let next = Book {
            default: (id != LOCAL_ID).then(|| id.to_owned()),
            instances: st.book.instances.clone(),
        };
        self.save(&next)?;
        st.book = next;
        Ok(())
    }

    /// Add an instance from a pairing link (`hokkabaz://pair?url=..&token=hkp_..`), the text a
    /// QR code carries.
    pub async fn pair(&self, uri: &str, name: Option<&str>) -> Result<InstanceDto, HkError> {
        let (url, token) = parse_pairing_uri(uri)?;
        let api_token = remote::exchange_pairing(&url, &token).await?;
        self.add(name.unwrap_or_default(), &url, api_token.expose())
    }

    pub async fn status(&self, id: &str) -> Result<InstanceStatusDto, HkError> {
        if id == LOCAL_ID {
            return Ok(InstanceStatusDto {
                id: LOCAL_ID.to_owned(),
                state: InstanceStateDto::Online,
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                latency_ms: Some(0),
                message: None,
            });
        }
        let url = self.stored(id)?.base_url;
        let token = self.secrets.get(&token_name(id))?;
        Ok(remote::probe(id, &url, token.as_ref()).await)
    }

    fn stored(&self, id: &str) -> Result<Stored, HkError> {
        self.lock()
            .book
            .instances
            .iter()
            .find(|i| i.id == id)
            .cloned()
            .ok_or_else(|| not_found(id))
    }

    /// Switch the active core to `id`; chat/settings calls from now on hit it. A remote must
    /// answer its probe first so a dead URL cannot strand the UI. Also becomes the start-up
    /// default.
    pub async fn connect(&self, id: &str) -> Result<InstanceDto, HkError> {
        let _g = self.connect_lock.lock().await;
        if id == LOCAL_ID {
            if let Some(old) = handle::activate(handle::local()?) {
                old.shutdown().await;
            }
        } else {
            let stored = self.stored(id)?;
            let token = self.secrets.get(&token_name(id))?.ok_or_else(|| {
                HkError::new(
                    HkErrorKind::Unauthorized,
                    "no token stored for this instance",
                )
            })?;
            let st = remote::probe(id, &stored.base_url, Some(&token)).await;
            let msg = st.message.unwrap_or_default();
            match st.state {
                InstanceStateDto::Online => {}
                InstanceStateDto::Unauthorized => {
                    return Err(HkError::new(HkErrorKind::Unauthorized, msg));
                }
                InstanceStateDto::Unreachable => {
                    return Err(HkError::new(HkErrorKind::Unavailable, msg));
                }
            }
            let core = RemoteCore::new(&stored.base_url, token, self.profile)?;
            if let Some(old) = handle::activate(core) {
                old.shutdown().await;
            }
        }
        self.lock().connected = id.to_owned();
        self.set_default(id)?;
        self.get(id)
    }
}

fn host_label(url: &reqwest::Url) -> String {
    let host = url.host_str().unwrap_or("instance");
    url.port()
        .map_or_else(|| host.to_owned(), |p| format!("{host}:{p}"))
}

fn new_id(book: &Book) -> String {
    use std::hash::{BuildHasher, Hasher};
    loop {
        // RandomState is seeded per process from the OS; ids only need to be unique, not secret.
        let n = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let id = format!("inst_{:08x}", n as u32);
        if !book.instances.iter().any(|i| i.id == id) {
            return id;
        }
    }
}

/// `(instance base URL, pairing token)` from a pairing link.
pub fn parse_pairing_uri(uri: &str) -> Result<(String, String), HkError> {
    let u = reqwest::Url::parse(uri.trim()).map_err(|_| HkError::invalid("not a pairing link"))?;
    if u.scheme() != "hokkabaz" || u.host_str() != Some("pair") {
        return Err(HkError::invalid(
            "not a pairing link (expected hokkabaz://pair?url=..&token=..)",
        ));
    }
    let get = |k: &str| {
        u.query_pairs()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.into_owned())
            .filter(|v| !v.is_empty())
    };
    match (get("url"), get("token")) {
        (Some(url), Some(token)) => Ok((url, token)),
        _ => Err(HkError::invalid("pairing link lacks `url` or `token`")),
    }
}

static MANAGER: RwLock<Option<Arc<Manager>>> = RwLock::new(None);

pub(crate) fn set_global(m: Manager) {
    *MANAGER.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(m));
}

pub(crate) fn clear_global() {
    *MANAGER.write().unwrap_or_else(PoisonError::into_inner) = None;
}

pub fn manager() -> Result<Arc<Manager>, HkError> {
    MANAGER
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| HkError::new(HkErrorKind::NotInitialized, "core_init has not been called"))
}

#[cfg(test)]
mod tests {
    use hk_store::MemorySecretStore;

    use super::*;

    fn mgr(dir: &Path) -> Manager {
        Manager::open(
            dir,
            CoreProfile::Desktop,
            Arc::new(MemorySecretStore::default()),
        )
    }

    #[test]
    fn pairing_uri_parses() {
        let (u, t) =
            parse_pairing_uri("hokkabaz://pair?url=http%3A%2F%2Fh%3A1&token=hkp_run.1.2.ab")
                .unwrap();
        assert_eq!((u.as_str(), t.as_str()), ("http://h:1", "hkp_run.1.2.ab"));
        assert!(parse_pairing_uri("https://h/pair?url=x&token=y").is_err());
        assert!(parse_pairing_uri("hokkabaz://pair?url=x").is_err());
        assert!(parse_pairing_uri("garbage").is_err());
    }

    #[test]
    fn book_persists_without_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let m = mgr(dir.path());
        let a = m.add("", "http://10.0.0.5:7878/", " tok ").unwrap();
        assert_eq!(
            (a.name.as_str(), a.base_url.as_deref(), a.has_token),
            ("10.0.0.5:7878", Some("http://10.0.0.5:7878"), true)
        );
        assert_eq!(
            m.add("x", "http://10.0.0.5:7878", "t").unwrap_err().kind,
            HkErrorKind::Conflict
        );
        assert!(m.add("x", "ftp://h", "t").is_err());
        assert!(m.add("x", "http://h", "  ").is_err());
        m.select(&a.id).unwrap();
        let raw = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(!raw.contains("tok"), "token must not reach instances.json");

        // reopen: same book; local first and default flag follows selection
        let m2 = Manager::open(
            dir.path(),
            CoreProfile::Desktop,
            Arc::new(MemorySecretStore::default()),
        );
        let l = m2.list();
        assert_eq!(l.len(), 2);
        assert_eq!(
            (l[0].id.as_str(), l[0].connected, l[0].is_default),
            (LOCAL_ID, true, false)
        );
        assert!(
            l[1].is_default && !l[1].connected && !l[1].has_token,
            "fresh secret store has no token"
        );
        assert_eq!(m2.select("nope").unwrap_err().kind, HkErrorKind::NotFound);
    }
}
