//! The Video Studio of this instance, served to remote apps through hk-server's `StudioApi`.
//! The same [`StudioHost`] the desktop app uses in-process, so a phone drives exactly the
//! pipeline a desktop would.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures::stream::BoxStream;
use hk_llm::{Provider, Router};
use hk_server::{ApiError, StudioApi};
use hk_store::{SecretStore, Store};
use hk_studio::host::{AppBackends, SETTINGS_KEY, StudioHost, StudioSettings};
use hk_studio::{StudioError, StudioService};
use serde_json::Value;

/// Build the Studio under `<data>/studio`. `provider` is the daemon's model provider; it is put
/// behind a router so Studio settings can name it as `provider/model`.
pub async fn build(
    data: &Path,
    store: Store,
    secrets: Arc<dyn SecretStore>,
    provider: Arc<dyn Provider>,
) -> anyhow::Result<Arc<StudioHost>> {
    let settings = Arc::new(RwLock::new(StudioSettings::load(
        store.get_setting(SETTINGS_KEY).await?.as_deref(),
    )));
    let router: Arc<dyn Provider> = Arc::new(Router::new().provider(provider));
    let svc = Arc::new(StudioService::new(
        data.join("studio"),
        hk_media::Tools::from_env(),
        Arc::new(AppBackends {
            provider: router,
            secrets: secrets.clone(),
            settings: settings.clone(),
            data_dir: data.to_path_buf(),
        }),
    ));
    Ok(Arc::new(StudioHost {
        svc,
        settings,
        store,
        secrets,
    }))
}

pub struct Api(pub Arc<StudioHost>);

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("studio_api::Api")
    }
}

fn api_err(e: StudioError) -> ApiError {
    match e {
        StudioError::NotFound(m) => ApiError::NotFound(m),
        StudioError::Invalid(m) => ApiError::BadRequest(m),
        other => ApiError::Internal(other.to_string()),
    }
}

#[async_trait]
impl StudioApi for Api {
    async fn call(&self, method: &str, params: Value) -> Result<Value, ApiError> {
        self.0.call(method, params).await.map_err(api_err)
    }

    async fn events(&self, id: &str) -> Result<BoxStream<'static, String>, ApiError> {
        self.0.events(id).await.map_err(api_err)
    }

    fn media_roots(&self) -> Vec<PathBuf> {
        self.0.media_roots()
    }

    fn upload_dir(&self) -> PathBuf {
        self.0.upload_dir()
    }
}
