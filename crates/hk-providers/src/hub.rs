//! [`ProviderHub`]: persisted provider settings, the registry built from them and the router the
//! agents hold. Changes rebuild the registry and swap the router behind a [`Reloadable`], so live
//! agents pick new providers on their next request without a restart.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use hk_llm::catalog::{Catalog, CatalogStore};
use hk_llm::registry::{KeyRef, ProviderRegistry, RouteConfig, build_router};
use hk_llm::{CancellationToken, ChatRequest, EventStream, ModelInfo, Provider, Router};
use hk_store::{Secret, SecretStore, Store};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::ProvidersError;
use crate::settings::{
    self, ProviderInput, Stored, canonical_kind, env_key, id_of_key_name, key_name,
    registry_error_provider, validate_model_ref,
};

/// Provider whose target can be swapped while agents hold it.
struct Reloadable(RwLock<Arc<dyn Provider>>);

impl Reloadable {
    fn current(&self) -> Arc<dyn Provider> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl Provider for Reloadable {
    fn id(&self) -> &str {
        "hk-core"
    }

    async fn models(&self) -> hk_llm::Result<Vec<ModelInfo>> {
        self.current().models().await
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> hk_llm::Result<EventStream> {
        self.current().stream(req, cancel).await
    }
}

/// On-device provider that ends the default model's fallback chain.
pub struct Offline {
    pub provider_id: String,
    /// Model id of the chosen fallback, if one is set and installed.
    pub model: Arc<dyn Fn() -> Option<String> + Send + Sync>,
}

impl std::fmt::Debug for Offline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offline")
            .field("provider_id", &self.provider_id)
            .finish_non_exhaustive()
    }
}

pub struct HubOptions {
    pub secrets: Arc<dyn SecretStore>,
    /// Always present in the registry (tests, the on-device provider, plugins).
    pub extras: Vec<Arc<dyn Provider>>,
    /// Where the models.dev catalog is cached.
    pub catalog_path: PathBuf,
    pub offline: Option<Offline>,
    /// Daemon mode: keys and endpoints may come from the environment.
    pub env: bool,
}

impl std::fmt::Debug for HubOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubOptions")
            .field("env", &self.env)
            .finish_non_exhaustive()
    }
}

struct Built {
    registry: ProviderRegistry,
    /// Provider id -> why it is unusable.
    errors: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderView {
    pub id: String,
    pub kind: String,
    pub base_url: Option<String>,
    pub enabled: bool,
    pub has_key: bool,
    pub builtin: bool,
    pub models: Vec<String>,
    /// Why the provider is not usable right now (missing key, bad endpoint), if so.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsView {
    pub providers: Vec<ProviderView>,
    pub default_model: Option<String>,
    pub plan_mode: bool,
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsView {
    pub models: Vec<ModelInfo>,
    /// `(provider id, message)`; the rest of the providers are still usable.
    pub errors: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct Preferences {
    pub default_model: Option<String>,
    pub plan_mode: bool,
    /// Already validated / created by the caller.
    pub workspace_root: Option<String>,
}

pub struct ProviderHub {
    store: Store,
    secrets: Arc<dyn SecretStore>,
    /// Serialises settings read-modify-write.
    settings: Mutex<Stored>,
    built: RwLock<Arc<Built>>,
    catalog: RwLock<Option<Arc<Catalog>>>,
    catalog_store: CatalogStore,
    reload: Arc<Reloadable>,
    extras: Vec<Arc<dyn Provider>>,
    offline: Option<Offline>,
    env: bool,
}

impl std::fmt::Debug for ProviderHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProviderHub")
    }
}

impl ProviderHub {
    pub async fn open(store: Store, opts: HubOptions) -> Result<Arc<Self>, ProvidersError> {
        let stored = Stored::load(
            store.get_setting(settings::SETTINGS_KEY).await?.as_deref(),
            opts.env,
        )?;
        let catalog_store = CatalogStore::new(opts.catalog_path);
        let catalog = catalog_store.load_cached().map(Arc::new);
        let hub = Arc::new(Self {
            store,
            secrets: opts.secrets,
            settings: Mutex::new(stored.clone()),
            built: RwLock::new(Arc::new(Built {
                registry: ProviderRegistry::new(),
                errors: HashMap::new(),
            })),
            catalog: RwLock::new(catalog),
            catalog_store,
            reload: Arc::new(Reloadable(RwLock::new(Arc::new(Router::new())))),
            extras: opts.extras,
            offline: opts.offline,
            env: opts.env,
        });
        hub.rebuild(&stored)?;
        Ok(hub)
    }

    /// What agents (and Studio) call. Follows every later settings change.
    pub fn provider(&self) -> Arc<dyn Provider> {
        self.reload.clone()
    }

    pub fn catalog_is_stale(&self) -> bool {
        !self.catalog_store.is_fresh()
    }

    pub async fn stored(&self) -> Stored {
        self.settings.lock().await.clone()
    }

    pub async fn default_model(&self) -> Option<String> {
        self.settings.lock().await.default_model.clone()
    }

    fn has_key(&self, provider_id: &str) -> bool {
        matches!(self.secrets.get(&key_name(provider_id)), Ok(Some(_)))
            || (self.env && env_key(provider_id).is_some())
    }

    /// Rebuild providers from `stored` (lenient: one bad provider does not hide the rest).
    pub fn rebuild(&self, stored: &Stored) -> Result<(), ProvidersError> {
        let configs = settings::to_configs(stored, |id| self.has_key(id));
        let secrets = self.secrets.clone();
        let env = self.env;
        let keys = move |r: &KeyRef| {
            secrets
                .get(&r.0)
                .ok()
                .flatten()
                .map(|s| s.expose().to_owned())
                .or_else(|| {
                    env.then(|| id_of_key_name(&r.0).and_then(env_key))
                        .flatten()
                })
        };
        let (mut registry, errs) = ProviderRegistry::build_lenient(&configs, &keys);
        for p in &self.extras {
            registry = registry.with_provider(p.clone());
        }
        let errors = errs
            .iter()
            .map(|e| (registry_error_provider(e).to_owned(), e.to_string()))
            .collect::<HashMap<_, _>>();
        // Offline fallback: the default model's chain ends in the chosen on-device model.
        let mut routes = Vec::new();
        if let (Some(off), Some(def)) = (&self.offline, stored.default_model.clone())
            && let Some(local) = (off.model)()
            && !def.starts_with(&format!("{}/", off.provider_id))
            && registry.providers().any(|p| p.id() == off.provider_id)
        {
            routes.push(RouteConfig {
                key: Some(def.clone()),
                refs: vec![def, format!("{}/{local}", off.provider_id)],
            });
        }
        let mut router =
            build_router(&registry, routes).map_err(|e| ProvidersError::Provider(e.to_string()))?;
        if let Some(c) = self
            .catalog
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            router = router.catalog(c);
        }
        *self
            .reload
            .0
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(router);
        *self.built.write().unwrap_or_else(PoisonError::into_inner) =
            Arc::new(Built { registry, errors });
        Ok(())
    }

    /// Rebuild with the current settings (the on-device model set changed).
    pub async fn rebuild_current(&self) {
        let s = self.settings.lock().await.clone();
        if let Err(e) = self.rebuild(&s) {
            tracing::warn!(error = %e, "provider rebuild failed");
        }
    }

    fn view_of(&self, s: &Stored) -> SettingsView {
        let built = self
            .built
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        SettingsView {
            providers: s
                .providers
                .iter()
                .map(|p| ProviderView {
                    id: p.id.clone(),
                    kind: canonical_kind(&p.kind).to_owned(),
                    base_url: p.base_url.clone(),
                    enabled: p.enabled,
                    has_key: self.has_key(&p.id),
                    builtin: settings::is_builtin(&p.id),
                    models: p.models.clone(),
                    error: p
                        .enabled
                        .then(|| built.errors.get(&p.id).cloned())
                        .flatten(),
                })
                .collect(),
            default_model: s.default_model.clone(),
            plan_mode: s.is_plan(),
            workspace_root: s.workspace_root.clone(),
        }
    }

    /// Persist, rebuild providers, return the new view.
    async fn commit(&self, s: &Stored) -> Result<SettingsView, ProvidersError> {
        self.store
            .set_setting(settings::SETTINGS_KEY, &serde_json::to_string(s)?)
            .await?;
        self.rebuild(s)?;
        Ok(self.view_of(s))
    }

    pub async fn view(&self) -> SettingsView {
        self.view_of(&*self.settings.lock().await)
    }

    pub async fn save_provider(
        &self,
        input: ProviderInput,
    ) -> Result<SettingsView, ProvidersError> {
        let mut s = self.settings.lock().await;
        let mut next = s.clone();
        next.upsert(input)?;
        let v = self.commit(&next).await?;
        *s = next;
        Ok(v)
    }

    pub async fn remove_provider(&self, id: &str) -> Result<SettingsView, ProvidersError> {
        let mut s = self.settings.lock().await;
        let mut next = s.clone();
        next.remove(id)?;
        // Drop the orphaned key too; a failure here must not block removal.
        if let Err(e) = self
            .store
            .delete_secret(&*self.secrets, &key_name(id))
            .await
        {
            tracing::warn!(provider = %id, error = %e, "could not delete provider key");
        }
        let v = self.commit(&next).await?;
        *s = next;
        Ok(v)
    }

    pub async fn set_api_key(
        &self,
        provider_id: &str,
        key: Option<&str>,
    ) -> Result<SettingsView, ProvidersError> {
        let s = self.settings.lock().await;
        if s.find(provider_id).is_none() {
            return Err(ProvidersError::NotFound(format!(
                "no provider `{provider_id}`"
            )));
        }
        let name = key_name(provider_id);
        match key.map(str::trim).filter(|k| !k.is_empty()) {
            Some(k) => {
                self.store
                    .put_secret(&*self.secrets, &name, Secret::new(k))
                    .await?
            }
            None => self.store.delete_secret(&*self.secrets, &name).await?,
        }
        self.rebuild(&s)?;
        Ok(self.view_of(&s))
    }

    pub async fn set_preferences(
        &self,
        prefs: Preferences,
    ) -> Result<SettingsView, ProvidersError> {
        let mut s = self.settings.lock().await;
        let mut next = s.clone();
        if let Some(m) = &prefs.default_model {
            validate_model_ref(m)?;
        }
        next.default_model = prefs.default_model;
        next.set_plan(prefs.plan_mode);
        next.workspace_root = prefs.workspace_root;
        let v = self.commit(&next).await?;
        *s = next;
        Ok(v)
    }

    pub async fn list_models(&self) -> ModelsView {
        let built = self
            .built
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let catalog = self
            .catalog
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let list = built.registry.list_models(catalog.as_deref()).await;
        let mut errors: Vec<(String, String)> = built
            .errors
            .iter()
            .map(|(id, m)| (id.clone(), m.clone()))
            .collect();
        errors.sort();
        errors.extend(list.errors.into_iter().map(|(id, e)| (id, e.to_string())));
        ModelsView {
            models: list.models,
            errors,
        }
    }

    pub async fn refresh_catalog(&self) -> Result<(), ProvidersError> {
        let client = hk_llm::openai::default_client()
            .map_err(|e| ProvidersError::Provider(e.to_string()))?;
        let cat = self
            .catalog_store
            .refresh(&client)
            .await
            .map_err(|e| ProvidersError::Provider(format!("catalog: {e}")))?;
        *self.catalog.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(cat));
        self.rebuild_current().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use hk_store::MemorySecretStore;

    use super::*;

    async fn hub() -> (Arc<ProviderHub>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let hub = ProviderHub::open(
            Store::open_in_memory().await.unwrap(),
            HubOptions {
                secrets: Arc::new(MemorySecretStore::default()),
                extras: Vec::new(),
                catalog_path: dir.path().join("models.json"),
                offline: None,
                env: false,
            },
        )
        .await
        .unwrap();
        (hub, dir)
    }

    fn lab() -> ProviderInput {
        ProviderInput {
            id: "lab".into(),
            kind: "openai_compat".into(),
            base_url: Some("http://127.0.0.1:9/v1".into()),
            enabled: true,
            models: vec!["m1".into()],
        }
    }

    #[tokio::test]
    async fn missing_key_is_reported_then_fixed() {
        let (hub, _d) = hub().await;
        let v = hub.view().await;
        let ds = v.providers.iter().find(|p| p.id == "deepseek").unwrap();
        assert!(ds.enabled && !ds.has_key);
        assert!(ds.error.is_some(), "deepseek needs a key");

        hub.set_api_key("deepseek", Some(" sk-test "))
            .await
            .unwrap();
        let v = hub.view().await;
        let ds = v.providers.iter().find(|p| p.id == "deepseek").unwrap();
        assert!(ds.has_key && ds.error.is_none());
        assert!(hub.set_api_key("nope", Some("k")).await.is_err());
    }

    #[tokio::test]
    async fn custom_provider_lists_fixed_models_and_persists() {
        let (hub, _d) = hub().await;
        hub.save_provider(lab()).await.unwrap();
        let m = hub.list_models().await;
        assert!(m.models.iter().any(|m| m.provider == "lab" && m.id == "m1"));

        hub.set_preferences(Preferences {
            default_model: Some("lab/m1".into()),
            plan_mode: true,
            workspace_root: None,
        })
        .await
        .unwrap();
        assert!(
            hub.set_preferences(Preferences {
                default_model: Some("no-slash".into()),
                plan_mode: false,
                workspace_root: None,
            })
            .await
            .is_err()
        );
        let v = hub.remove_provider("lab").await.unwrap();
        assert!(v.providers.iter().all(|p| p.id != "lab"));
        assert!(hub.remove_provider("anthropic").await.is_err());
    }
}
