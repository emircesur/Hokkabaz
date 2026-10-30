//! On-device models: the GGUF store, the `local` provider and the offline-fallback setting.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use hk_llm::Provider;
use hk_local::{CatalogEntry, Device, ModelStore, catalog};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::error::{HkError, HkErrorKind};

const FALLBACK_KEY: &str = "local.fallback";
const GPU_KEY: &str = "local.gpu";

pub type Rebuild =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

pub struct Ctx {
    pub store: ModelStore,
    rebuild: Mutex<Option<Rebuild>>,
    pub db: hk_store::Store,
    fallback: RwLock<Option<String>>,
    downloads: Mutex<HashMap<String, CancellationToken>>,
    progress: Mutex<HashMap<String, hk_local::DownloadProgress>>,
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("local_models::Ctx")
    }
}

static CTX: RwLock<Option<Arc<Ctx>>> = RwLock::new(None);

pub async fn install(data_dir: &Path, db: hk_store::Store) -> Result<Arc<Ctx>, HkError> {
    let fallback = db
        .get_setting(FALLBACK_KEY)
        .await?
        .filter(|s| !s.is_empty());
    if let Some(g) = db
        .get_setting(GPU_KEY)
        .await?
        .and_then(|s| serde_json::from_str::<hk_local::gpu::GpuSettings>(&s).ok())
    {
        hk_local::gpu::set(g);
    }
    let c = Arc::new(Ctx {
        store: ModelStore::new(data_dir.join("models")),
        db,
        rebuild: Mutex::new(None),
        fallback: RwLock::new(fallback),
        downloads: Mutex::default(),
        progress: Mutex::default(),
    });
    *CTX.write().unwrap_or_else(PoisonError::into_inner) = Some(c.clone());
    Ok(c)
}

pub fn uninstall() {
    if let Some(c) = CTX.write().unwrap_or_else(PoisonError::into_inner).take() {
        for t in c
            .downloads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
        {
            t.cancel();
        }
    }
}

pub fn ctx() -> Result<Arc<Ctx>, HkError> {
    CTX.read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| {
            HkError::new(
                HkErrorKind::NotInitialized,
                "on-device models need the in-process core (core_init)",
            )
        })
}

/// Model id of the on-device fallback, if set and still installed.
pub fn fallback() -> Option<String> {
    let c = CTX.read().unwrap_or_else(PoisonError::into_inner).clone()?;
    let id = c
        .fallback
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()?;
    c.store.get(&id).map(|_| id)
}

/// The `local` provider for the router (None when this build has no inference engine).
pub fn provider(c: &Ctx) -> Option<Arc<dyn Provider>> {
    #[cfg(not(target_os = "android"))]
    {
        Some(Arc::new(hk_local::engine::LlamaProvider::new(
            c.store.clone(),
        )))
    }
    #[cfg(target_os = "android")]
    {
        let _ = c;
        None
    }
}

pub const ENGINE: bool = cfg!(not(target_os = "ios"));

fn fit_str(f: catalog::Fit) -> &'static str {
    match f {
        catalog::Fit::Good => "good",
        catalog::Fit::Tight => "tight",
        catalog::Fit::TooBig => "too_big",
    }
}

fn entry_json(
    e: &CatalogEntry,
    dev: &Device,
    c: &Ctx,
    installed: &[hk_local::InstalledModel],
) -> Value {
    let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
    v["fit"] = json!(fit_str(catalog::fit(e.size_bytes, dev)));
    v["ram_needed"] = json!(catalog::ram_needed(e.size_bytes));
    v["installed"] = json!(installed.iter().any(|m| m.id == e.id));
    let p = c
        .progress
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&e.id)
        .copied();
    v["downloading"] = json!(
        c.downloads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&e.id)
    );
    v["progress"] = json!(p.map(|p| json!({"done": p.done, "total": p.total})));
    v
}

impl Ctx {
    pub fn overview(&self) -> Value {
        let dev = Device::probe();
        let installed = self.store.installed();
        let models: Vec<Value> = catalog::builtin()
            .iter()
            .map(|e| entry_json(e, &dev, self, &installed))
            .collect();
        let custom: Vec<&hk_local::InstalledModel> =
            installed.iter().filter(|m| m.custom).collect();
        json!({
            "engine": ENGINE,
            "device": {"total_ram": dev.total_ram, "available_ram": dev.available_ram, "cores": dev.cores},
            "recommended": catalog::recommend(&dev).map(|m| m.id),
            "models": models,
            "custom": custom,
            "fallback": self.fallback.read().unwrap_or_else(PoisonError::into_inner).clone(),
            "loaded": hk_local::loaded_models(),
            "gpu": {"backend": hk_local::gpu::BACKEND, "settings": hk_local::gpu::get(), "devices": hk_local::gpu::devices()},
        })
    }

    /// GPU offload for local models; saved, and used from the next model load on.
    pub async fn set_gpu(&self, g: hk_local::gpu::GpuSettings) -> Result<(), HkError> {
        let raw = serde_json::to_string(&g).map_err(|e| HkError::internal(e.to_string()))?;
        self.db.set_setting(GPU_KEY, &raw).await?;
        hk_local::gpu::set(g);
        Ok(())
    }

    pub async fn set_fallback(&self, id: Option<String>) -> Result<(), HkError> {
        if let Some(i) = &id
            && self.store.get(i).is_none()
        {
            return Err(HkError::invalid(format!("model `{i}` is not downloaded")));
        }
        self.db
            .set_setting(FALLBACK_KEY, id.as_deref().unwrap_or(""))
            .await?;
        *self
            .fallback
            .write()
            .unwrap_or_else(PoisonError::into_inner) = id;
        Ok(())
    }

    /// Run a download to completion, reporting progress; one per model at a time.
    pub async fn download(
        &self,
        id: &str,
        on_progress: impl Fn(hk_local::DownloadProgress) + Send + Sync,
    ) -> Result<(), HkError> {
        let entry = catalog::builtin()
            .into_iter()
            .find(|m| m.id == id)
            .ok_or_else(|| HkError::new(HkErrorKind::NotFound, format!("unknown model `{id}`")))?;
        let token = CancellationToken::new();
        self.downloads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned(), token.clone());
        let res = self
            .store
            .download(
                &entry,
                |p| {
                    self.progress
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(entry.id.clone(), p);
                    on_progress(p);
                },
                token,
            )
            .await;
        self.downloads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        self.progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        res.map(|_| ()).map_err(|e| match e {
            hk_local::LocalError::Cancelled => {
                HkError::new(HkErrorKind::Conflict, "download cancelled")
            }
            other => HkError::new(HkErrorKind::Unavailable, other.to_string()),
        })
    }

    /// Called by the core once it can rebuild its providers (after a fallback change).
    pub fn set_rebuild(&self, f: Rebuild) {
        *self.rebuild.lock().unwrap_or_else(PoisonError::into_inner) = Some(f);
    }

    pub async fn rebuild_providers(&self) {
        let f = self
            .rebuild
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(f) = f {
            f().await;
        }
    }

    pub fn cancel(&self, id: &str) {
        if let Some(t) = self
            .downloads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
        {
            t.cancel();
        }
    }
}
