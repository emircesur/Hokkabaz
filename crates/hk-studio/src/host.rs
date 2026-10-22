//! The Studio as an application service: preferences, keys, and one `call(method, params)` entry
//! point that every front end goes through (the in-process app, and the daemon's HTTP API for
//! remote apps). Keeping a single dispatcher means a phone driving a server's Studio does exactly
//! what the desktop does locally.
//!
//! Methods take and return JSON. Paths in results are paths on the machine running the Studio.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use futures::stream::BoxStream;
use hk_llm::registry::KeyRef;
use hk_llm::{Provider, Route};
use hk_store::{Secret, SecretStore, Store};
use hk_stt::{AudioSplitter, SttConfig, SttProvider, TranscribeOpts, Transcriber};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::broll::{BrollOpts, LocalFolder, Pexels, Pixabay, StockProvider};
use crate::edit::EditOp;
use crate::highlights::HighlightOpts;
use crate::llm::{Llm, ProviderLlm};
use crate::social::{Platform, SocialOpts};
use crate::{Backends, RenderOpts, Result, StudioError, StudioService};

pub const SETTINGS_KEY: &str = "studio.settings";
pub const KEY_STT: &str = "studio/stt_key";
pub const KEY_PEXELS: &str = "studio/pexels_key";
pub const KEY_PIXABAY: &str = "studio/pixabay_key";
pub const KEY_YT_SECRET: &str = "studio/youtube_secret";
pub const KEY_YT_REFRESH: &str = "studio/youtube_refresh";

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
    /// Google OAuth client id for YouTube uploads (secret + refresh token are in the secret store).
    pub youtube_client_id: Option<String>,
}

impl StudioSettings {
    pub fn load(raw: Option<&str>) -> Self {
        raw.and_then(|r| serde_json::from_str(r).ok())
            .unwrap_or_default()
    }

    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(StudioError::Invalid(m));
        if let Some(m) = &self.llm_model
            && Route::parse(m).is_err()
        {
            return bad(format!("model must be `provider/model`, got `{m}`"));
        }
        match self.stt_provider.as_deref() {
            None | Some("openai_compat" | "whisper_local") => {}
            Some(o) => return bad(format!("unknown stt_provider `{o}`")),
        }
        if let Some(u) = &self.stt_base_url
            && !(u.starts_with("http://") || u.starts_with("https://"))
        {
            return bad("stt_base_url must start with http:// or https://".into());
        }
        Ok(())
    }
}

pub type SharedSettings = Arc<RwLock<StudioSettings>>;

/// Wires the service to the app's model router, secret store and settings.
pub struct AppBackends {
    pub provider: Arc<dyn Provider>,
    pub secrets: Arc<dyn SecretStore>,
    pub settings: SharedSettings,
    pub data_dir: PathBuf,
}

impl std::fmt::Debug for AppBackends {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppBackends")
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

impl AppBackends {
    fn snapshot(&self) -> StudioSettings {
        self.settings
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn secret(&self, name: &str) -> Option<String> {
        self.secrets
            .get(name)
            .ok()
            .flatten()
            .map(|s| s.expose().to_owned())
    }
}

#[async_trait]
impl Backends for AppBackends {
    async fn llm(&self) -> Result<Arc<dyn Llm>> {
        let model = self.snapshot().llm_model.ok_or_else(|| {
            StudioError::Invalid(
                "choose the Studio model first (Studio settings, `provider/model`)".into(),
            )
        })?;
        Ok(Arc::new(ProviderLlm::new(self.provider.clone(), model)))
    }

    async fn transcriber(
        &self,
        splitter: Arc<dyn AudioSplitter>,
    ) -> Result<(Arc<dyn Transcriber>, TranscribeOpts)> {
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
        cfg.model_path = s
            .whisper_model_path
            .filter(|p| !p.is_empty())
            .map(PathBuf::from);
        if self.secret(KEY_STT).is_some() {
            cfg.key_ref = Some(KeyRef::new(KEY_STT));
        }
        let secrets = self.secrets.clone();
        let keys = move |r: &KeyRef| {
            secrets
                .get(&r.0)
                .ok()
                .flatten()
                .map(|s| s.expose().to_owned())
        };
        let t = hk_stt::build_chunked_transcriber(&cfg, &keys, splitter)?;
        Ok((
            t,
            TranscribeOpts {
                language: cfg.language,
                prompt: None,
                word_timestamps: true,
            },
        ))
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

    fn youtube(&self) -> Option<crate::publish::YoutubeCreds> {
        Some(crate::publish::YoutubeCreds {
            client_id: self
                .snapshot()
                .youtube_client_id
                .filter(|c| !c.is_empty())?,
            client_secret: self.secret(KEY_YT_SECRET)?,
            refresh_token: self.secret(KEY_YT_REFRESH)?,
        })
    }

    fn face_model_dir(&self) -> Option<PathBuf> {
        Some(self.data_dir.join("models"))
    }
}

pub fn put_secret(store: &dyn SecretStore, name: &str, value: Option<&str>) -> Result<()> {
    let r = match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => store.set(name, &Secret::new(v)),
        None => store.delete(name),
    };
    r.map_err(|e| StudioError::Invalid(format!("secret store: {e}")))
}

/// Stream of JSON `StudioEvent`s for one project.
pub type EventStream = BoxStream<'static, String>;

#[derive(Debug, Clone)]
pub struct StudioHost {
    pub svc: Arc<StudioService>,
    pub settings: SharedSettings,
    pub store: Store,
    pub secrets: Arc<dyn SecretStore>,
}

fn arg<T: serde::de::DeserializeOwned>(p: &Value, key: &str) -> Result<T> {
    serde_json::from_value(p.get(key).cloned().unwrap_or(Value::Null))
        .map_err(|e| StudioError::Invalid(format!("`{key}`: {e}")))
}

fn opt<T: serde::de::DeserializeOwned>(p: &Value, key: &str) -> Result<Option<T>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| StudioError::Invalid(format!("`{key}`: {e}"))),
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<Value> {
    Ok(serde_json::to_value(v)?)
}

impl StudioHost {
    async fn project_value(&self, id: &str) -> Result<Value> {
        let p = self.svc.open(id).await?.snapshot().await;
        let mut v = serde_json::to_value(&p)?;
        v["has_transcript"] = json!(p.transcript.is_some());
        Ok(v)
    }

    /// Run one Studio method. `params` is a JSON object; unknown methods are `Invalid`.
    #[allow(clippy::too_many_lines)]
    pub async fn call(&self, method: &str, p: Value) -> Result<Value> {
        let svc = &self.svc;
        match method {
            "check_tools" => Ok(check_tools().await),
            "get_settings" => {
                let s = self
                    .settings
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                let has = |n: &str| matches!(self.secrets.get(n), Ok(Some(_)));
                Ok(json!({
                    "settings": s,
                    "has_stt_key": has(KEY_STT), "has_pexels_key": has(KEY_PEXELS), "has_pixabay_key": has(KEY_PIXABAY),
                    "has_youtube_secret": has(KEY_YT_SECRET), "has_youtube_refresh": has(KEY_YT_REFRESH),
                }))
            }
            "save_settings" => {
                let s: StudioSettings = arg(&p, "settings")?;
                s.validate()?;
                self.store
                    .set_setting(SETTINGS_KEY, &serde_json::to_string(&s)?)
                    .await
                    .map_err(|e| StudioError::Invalid(e.to_string()))?;
                *self
                    .settings
                    .write()
                    .unwrap_or_else(PoisonError::into_inner) = s;
                Ok(Value::Null)
            }
            "set_key" => {
                let which: String = arg(&p, "which")?;
                let name = match which.as_str() {
                    "stt" => KEY_STT,
                    "pexels" => KEY_PEXELS,
                    "pixabay" => KEY_PIXABAY,
                    "youtube_secret" => KEY_YT_SECRET,
                    "youtube_refresh" => KEY_YT_REFRESH,
                    o => return Err(StudioError::Invalid(format!("unknown key `{o}`"))),
                };
                let key: Option<String> = opt(&p, "key")?;
                put_secret(self.secrets.as_ref(), name, key.as_deref())?;
                Ok(Value::Null)
            }
            "templates" => Ok(json!({
                "templates": crate::templates::builtin_templates(),
                "caption_presets": ["classic", "karaoke", "pop", "minimal"],
                "platforms": Platform::ALL.map(Platform::key),
            })),
            "projects" => to_value(&svc.projects().await?),
            "import" => {
                let path: String = arg(&p, "path")?;
                let name: Option<String> = opt(&p, "name")?;
                let live = svc.import(Path::new(&path), name.as_deref()).await?;
                let id = live.snapshot().await.id;
                self.project_value(&id).await
            }
            "project" => self.project_value(&arg::<String>(&p, "id")?).await,
            "delete" => {
                svc.delete(&arg::<String>(&p, "id")?).await?;
                Ok(Value::Null)
            }
            "transcribe" => {
                svc.transcribe(&arg::<String>(&p, "id")?).await?;
                Ok(Value::Null)
            }
            "analyze" => {
                let id: String = arg(&p, "id")?;
                let count: u32 = opt(&p, "count")?.unwrap_or(5);
                let brief: Option<String> = opt(&p, "brief")?;
                let template: Option<String> = opt(&p, "template")?;
                let opts = HighlightOpts {
                    count: count.clamp(1, 30) as usize,
                    min_secs: opt(&p, "min_secs")?.unwrap_or(20.0),
                    max_secs: opt(&p, "max_secs")?.unwrap_or(60.0),
                    brief: brief.filter(|b| !b.trim().is_empty()),
                    ..HighlightOpts::default()
                };
                to_value(&svc.analyze(&id, &opts, None, template.as_deref()).await?)
            }
            "edit" => {
                let id: String = arg(&p, "id")?;
                let ops: Vec<EditOp> = arg(&p, "ops")?;
                let by: String = opt(&p, "by")?.unwrap_or_else(|| "ui".into());
                let live = svc.open(&id).await?;
                let mut rev = live.rev().await;
                for op in ops {
                    rev = live.edit(op, &by).await?;
                }
                Ok(json!(rev))
            }
            "undo" => Ok(json!(
                svc.open(&arg::<String>(&p, "id")?).await?.undo().await?
            )),
            "redo" => Ok(json!(
                svc.open(&arg::<String>(&p, "id")?).await?.redo().await?
            )),
            "history" => {
                let live = svc.open(&arg::<String>(&p, "id")?).await?;
                Ok(
                    json!({"can_undo": live.can_undo().await, "can_redo": live.can_redo().await, "rev": live.rev().await}),
                )
            }
            "preview" => {
                let plan = svc
                    .open(&arg::<String>(&p, "id")?)
                    .await?
                    .preview(&arg::<String>(&p, "clip")?)
                    .await?;
                to_value(&plan)
            }
            "reframe" => {
                let square: bool = opt(&p, "square")?.unwrap_or(false);
                let base = if square {
                    crate::reframe::ReframeSpec::square()
                } else {
                    crate::reframe::ReframeSpec::vertical()
                };
                let n = svc
                    .track(
                        &arg::<String>(&p, "id")?,
                        &arg::<String>(&p, "clip")?,
                        Some(base),
                    )
                    .await?;
                Ok(json!(n))
            }
            "prepare_face_model" => {
                #[cfg(feature = "vision")]
                {
                    Ok(json!(svc.ensure_face_model().await?.is_some()))
                }
                #[cfg(not(feature = "vision"))]
                {
                    Ok(json!(false))
                }
            }
            "broll" => {
                let max: u32 = opt(&p, "max")?.unwrap_or(3);
                let opts = BrollOpts {
                    max: max.clamp(1, 10) as usize,
                    ..Default::default()
                };
                let n = svc
                    .add_broll(
                        &arg::<String>(&p, "id")?,
                        &arg::<String>(&p, "clip")?,
                        &opts,
                    )
                    .await?;
                Ok(json!(n))
            }
            "social" => {
                let platforms: Vec<Platform> = opt(&p, "platforms")?.unwrap_or_default();
                let opts = SocialOpts {
                    tone: opt(&p, "tone")?,
                    language: opt(&p, "language")?,
                    call_to_action: None,
                };
                let list: &[Platform] = if platforms.is_empty() {
                    &Platform::ALL
                } else {
                    &platforms
                };
                svc.social(
                    &arg::<String>(&p, "id")?,
                    &arg::<String>(&p, "clip")?,
                    list,
                    &opts,
                )
                .await?;
                Ok(Value::Null)
            }
            "render" => Ok(json!(
                svc.start_render(
                    &arg::<String>(&p, "id")?,
                    &arg::<String>(&p, "clip")?,
                    RenderOpts::default()
                )
                .await?
            )),
            "cancel_render" => Ok(json!(svc.cancel_render(&arg::<String>(&p, "job")?).await)),
            "render_path" => Ok(json!(
                svc.render_path(&arg::<String>(&p, "id")?, &arg::<String>(&p, "clip")?)
                    .await
                    .map(|p| p.to_string_lossy().into_owned())
            )),
            "publish_youtube" => {
                let privacy = match opt::<String>(&p, "privacy")?.as_deref() {
                    Some("public") => crate::publish::Privacy::Public,
                    Some("unlisted") => crate::publish::Privacy::Unlisted,
                    _ => crate::publish::Privacy::Private,
                };
                Ok(json!(
                    svc.publish_youtube(
                        &arg::<String>(&p, "id")?,
                        &arg::<String>(&p, "clip")?,
                        privacy
                    )
                    .await?
                    .url
                ))
            }
            "export" => {
                let dir: String = arg(&p, "dir")?;
                let out = svc
                    .export_package(
                        &arg::<String>(&p, "id")?,
                        &arg::<String>(&p, "clip")?,
                        Path::new(&dir),
                    )
                    .await?;
                Ok(json!(out.to_string_lossy()))
            }
            "thumbnail" => {
                let snap = svc.open(&arg::<String>(&p, "id")?).await?.snapshot().await;
                let at: f64 = opt(&p, "at")?.unwrap_or(0.0);
                Ok(json!(
                    svc.studio().thumbnail(&snap, at).await?.to_string_lossy()
                ))
            }
            "proxy" => {
                let snap = svc.open(&arg::<String>(&p, "id")?).await?.snapshot().await;
                Ok(json!(svc.studio().proxy(&snap).await?.to_string_lossy()))
            }
            other => Err(StudioError::Invalid(format!(
                "unknown Studio method `{other}`"
            ))),
        }
    }

    /// Live change feed of one project as JSON `StudioEvent`s; `{"kind":"resync"}` means events
    /// were missed and the project should be reloaded.
    pub async fn events(&self, id: &str) -> Result<EventStream> {
        let live = self.svc.open(id).await?;
        let rx = live.subscribe();
        Ok(Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            let msg = match rx.recv().await {
                Ok(ev) => {
                    serde_json::to_string(&ev).unwrap_or_else(|_| r#"{"kind":"resync"}"#.to_owned())
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    r#"{"kind":"resync"}"#.to_owned()
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            };
            Some((msg, rx))
        })))
    }

    /// Absolute paths a media request may read: the Studio's own folder and the B-roll folder.
    pub fn media_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.svc.studio().store.root().to_path_buf()];
        if let Some(d) = self
            .settings
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .broll_dir
            .clone()
        {
            roots.push(PathBuf::from(d));
        }
        roots
    }

    /// Where uploads for remote imports land.
    pub fn upload_dir(&self) -> PathBuf {
        self.svc.studio().store.root().join("_uploads")
    }
}

async fn check_tools() -> Value {
    let t = hk_media::Tools::from_env();
    let run = |bin: PathBuf| async move {
        #[cfg(target_os = "android")]
        {
            let _ = bin;
            None::<String>
        }
        #[cfg(not(target_os = "android"))]
        {
            tokio::process::Command::new(bin)
                .arg("-version")
                .output()
                .await
                .ok()
                .filter(|o| o.status.success())
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
        }
    };
    let (ff, fp) = (run(t.ffmpeg.clone()).await, run(t.ffprobe.clone()).await);
    json!({"ffmpeg": ff.is_some(), "ffprobe": fp.is_some(), "version": ff})
}
