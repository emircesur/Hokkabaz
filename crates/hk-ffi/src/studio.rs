//! Video Studio inside the in-process core: settings, model/keys plumbing and the shared
//! [`StudioService`] the `api::studio` functions and the agent's `studio_*` tools use.
//!
//! Studio runs on the machine that has ffmpeg (desktop, or a daemon). On Android the service
//! still starts, but ffmpeg calls fail with a clear error.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use hk_llm::registry::KeyRef;
use hk_llm::{Provider, Route};
use hk_store::{Secret, SecretStore};
use hk_stt::{AudioSplitter, SttConfig, SttProvider, TranscribeOpts, Transcriber};
use hk_studio::broll::{LocalFolder, Pexels, Pixabay, StockProvider};
use hk_studio::llm::{Llm, ProviderLlm};
use hk_studio::{Backends, StudioError, StudioService};
use serde::{Deserialize, Serialize};

use crate::error::{HkError, HkErrorKind};

pub const SETTINGS_KEY: &str = "studio.settings";
pub const KEY_STT: &str = "studio/stt_key";
pub const KEY_PEXELS: &str = "studio/pexels_key";
pub const KEY_PIXABAY: &str = "studio/pixabay_key";

/// Studio preferences (keys are in the secret store, never here).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StudioSettings {
    /// `provider/model` used for highlights, scoring, B-roll planning and copy.
    pub llm_model: Option<String>,
    /// `openai_compat` | `whisper_local`.
    pub stt_provider: Option<String>,
    pub stt_base_url: Option<String>,
    pub stt_model: Option<String>,
    pub stt_language: Option<String>,
    pub whisper_model_path: Option<String>,
    /// A folder of your own footage / images used as a B-roll source.
    pub broll_dir: Option<String>,
}

impl StudioSettings {
    pub fn load(raw: Option<&str>) -> Self {
        raw.and_then(|r| serde_json::from_str(r).ok()).unwrap_or_default()
    }

    pub fn validate(&self) -> Result<(), HkError> {
        if let Some(m) = &self.llm_model {
            Route::parse(m).map_err(|_| HkError::invalid(format!("model must be `provider/model`, got `{m}`")))?;
        }
        match self.stt_provider.as_deref() {
            None | Some("openai_compat" | "whisper_local") => {}
            Some(o) => return Err(HkError::invalid(format!("unknown stt_provider `{o}`"))),
        }
        if let Some(u) = &self.stt_base_url {
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                return Err(HkError::invalid("stt_base_url must start with http:// or https://"));
            }
        }
        Ok(())
    }
}

pub type SharedSettings = Arc<RwLock<StudioSettings>>;

pub struct FfiBackends {
    pub provider: Arc<dyn Provider>,
    pub secrets: Arc<dyn SecretStore>,
    pub settings: SharedSettings,
    pub data_dir: PathBuf,
}

impl std::fmt::Debug for FfiBackends {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiBackends").field("data_dir", &self.data_dir).finish_non_exhaustive()
    }
}

impl FfiBackends {
    fn snapshot(&self) -> StudioSettings {
        self.settings.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn secret(&self, name: &str) -> Option<String> {
        self.secrets.get(name).ok().flatten().map(|s| s.expose().to_owned())
    }
}

#[async_trait]
impl Backends for FfiBackends {
    async fn llm(&self) -> hk_studio::Result<Arc<dyn Llm>> {
        let model = self
            .snapshot()
            .llm_model
            .ok_or_else(|| StudioError::Invalid("choose the Studio model first (Studio settings, `provider/model`)".into()))?;
        Ok(Arc::new(ProviderLlm::new(self.provider.clone(), model)))
    }

    async fn transcriber(&self, splitter: Arc<dyn AudioSplitter>) -> hk_studio::Result<(Arc<dyn Transcriber>, TranscribeOpts)> {
        let s = self.snapshot();
        let mut cfg = SttConfig {
            provider: match s.stt_provider.as_deref() {
                Some("whisper_local") => SttProvider::WhisperLocal,
                _ => SttProvider::OpenaiCompat,
            },
            ..SttConfig::default()
        };
        if let Some(u) = s.stt_base_url.filter(|u| !u.is_empty()) {
            cfg.base_url = u.trim_end_matches('/').to_owned();
        }
        if let Some(m) = s.stt_model.filter(|m| !m.is_empty()) {
            cfg.model = m;
        }
        cfg.language = s.stt_language.clone().filter(|l| !l.is_empty());
        cfg.model_path = s.whisper_model_path.filter(|p| !p.is_empty()).map(PathBuf::from);
        if self.secret(KEY_STT).is_some() {
            cfg.key_ref = Some(KeyRef::new(KEY_STT));
        }
        let secrets = self.secrets.clone();
        let keys = move |r: &KeyRef| secrets.get(&r.0).ok().flatten().map(|s| s.expose().to_owned());
        let t = hk_stt::build_chunked_transcriber(&cfg, &keys, splitter)?;
        Ok((t, TranscribeOpts { language: cfg.language, prompt: None, word_timestamps: true }))
    }

    fn stock(&self) -> Vec<Arc<dyn StockProvider>> {
        let mut out: Vec<Arc<dyn StockProvider>> = Vec::new();
        if let Some(dir) = self.snapshot().broll_dir.filter(|d| Path::new(d).is_dir()) {
            out.push(Arc::new(LocalFolder::new(dir)));
        }
        if let Some(k) = self.secret(KEY_PEXELS) {
            out.push(Arc::new(Pexels::new(k)));
        }
        if let Some(k) = self.secret(KEY_PIXABAY) {
            out.push(Arc::new(Pixabay::new(k)));
        }
        out
    }

    fn face_model_dir(&self) -> Option<PathBuf> {
        Some(self.data_dir.join("models"))
    }
}

/// Everything `api::studio` needs, installed by `LocalCore::open`.
#[derive(Debug)]
pub struct Ctx {
    pub svc: Arc<StudioService>,
    pub settings: SharedSettings,
    pub store: hk_store::Store,
    pub secrets: Arc<dyn SecretStore>,
}

static CTX: RwLock<Option<Arc<Ctx>>> = RwLock::new(None);

/// Make this the process-wide Studio (called by `LocalCore::open`).
pub fn install(ctx: Ctx) {
    *CTX.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(ctx));
}

pub fn uninstall() {
    *CTX.write().unwrap_or_else(PoisonError::into_inner) = None;
}

pub fn ctx() -> Result<Arc<Ctx>, HkError> {
    CTX.read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| HkError::new(HkErrorKind::NotInitialized, "Studio needs the in-process core (core_init)"))
}

pub fn service() -> Result<Arc<StudioService>, HkError> {
    Ok(ctx()?.svc.clone())
}

pub fn studio_err(e: StudioError) -> HkError {
    match e {
        StudioError::NotFound(m) => HkError::new(HkErrorKind::NotFound, m),
        StudioError::Invalid(m) => HkError::invalid(m),
        StudioError::Llm(m) | StudioError::Http(m) => HkError::new(HkErrorKind::Provider, m),
        other => HkError::internal(other.to_string()),
    }
}

pub fn put_secret(store: &dyn SecretStore, name: &str, value: Option<&str>) -> Result<(), HkError> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => store.set(name, &Secret::new(v)).map_err(|e| HkError::internal(format!("secret store: {e}"))),
        None => store.delete(name).map_err(|e| HkError::internal(format!("secret store: {e}"))),
    }
}
