//! Provider selection config and builder.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hk_llm::{KeyRef, KeyResolver};
use serde::{Deserialize, Serialize};

use crate::chunked::{AudioSplitter, ChunkOpts, ChunkedTranscriber};
use crate::error::{Result, SttError};
use crate::openai_compat::{
    DEFAULT_MAX_UPLOAD_BYTES, OpenAiCompatOptions, OpenAiCompatTranscriber,
};
use crate::transcriber::Transcriber;

/// Which engine to use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SttProvider {
    /// `/audio/transcriptions` over HTTP (OpenAI, Groq, local servers).
    #[default]
    OpenaiCompat,
    /// whisper.cpp in-process (feature `whisper`).
    WhisperLocal,
}

/// Speech-to-text settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SttConfig {
    /// Engine.
    pub provider: SttProvider,
    /// API root incl. version, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    /// Model id (`whisper-1`, `whisper-large-v3`, ...).
    pub model: String,
    /// Secret reference for the bearer token; omit for keyless local servers.
    pub key_ref: Option<KeyRef>,
    /// Default language (ISO code); `None` = auto-detect.
    pub language: Option<String>,
    /// ggml model file for `whisper_local`.
    pub model_path: Option<PathBuf>,
    /// CPU threads for `whisper_local`.
    pub threads: Option<i32>,
    /// HTTP retries on 429/5xx.
    pub max_retries: u32,
    /// HTTP request timeout, seconds.
    pub timeout_secs: u64,
    /// Per-request upload cap in bytes (default 25 MiB).
    pub max_upload_bytes: u64,
    /// Extra HTTP headers.
    pub headers: BTreeMap<String, String>,
    /// Chunk length for long audio, seconds.
    pub chunk_secs: f64,
    /// Overlap between chunks, seconds.
    pub chunk_overlap_secs: f64,
    /// Concurrent chunk requests.
    pub concurrency: usize,
}

impl Default for SttConfig {
    fn default() -> Self {
        let c = ChunkOpts::default();
        Self {
            provider: SttProvider::default(),
            base_url: "https://api.openai.com/v1".into(),
            model: "whisper-1".into(),
            key_ref: None,
            language: None,
            model_path: None,
            threads: None,
            max_retries: 3,
            timeout_secs: 600,
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            headers: BTreeMap::new(),
            chunk_secs: c.chunk_secs,
            chunk_overlap_secs: c.overlap_secs,
            concurrency: c.concurrency,
        }
    }
}

impl SttConfig {
    /// Chunking parameters derived from this config.
    pub fn chunk_opts(&self) -> ChunkOpts {
        ChunkOpts {
            chunk_secs: self.chunk_secs,
            overlap_secs: self.chunk_overlap_secs,
            concurrency: self.concurrency,
            max_upload_bytes: Some(self.max_upload_bytes),
            ..ChunkOpts::default()
        }
    }
}

/// Build the engine selected by `cfg`. Key lookup happens here; the secret is
/// held only inside the transcriber.
pub fn build_transcriber(cfg: &SttConfig, keys: &dyn KeyResolver) -> Result<Arc<dyn Transcriber>> {
    match cfg.provider {
        SttProvider::OpenaiCompat => {
            let key = match &cfg.key_ref {
                Some(r) => Some(
                    keys.resolve(r)
                        .ok_or_else(|| SttError::MissingKey(r.0.clone()))?,
                ),
                None => None,
            };
            let opts = OpenAiCompatOptions {
                max_retries: cfg.max_retries,
                timeout: Duration::from_secs(cfg.timeout_secs),
                max_upload_bytes: cfg.max_upload_bytes,
                headers: cfg.headers.clone(),
                default_language: cfg.language.clone(),
                ..OpenAiCompatOptions::default()
            };
            Ok(Arc::new(OpenAiCompatTranscriber::new(
                &cfg.base_url,
                cfg.model.clone(),
                key,
                opts,
            )?))
        }
        SttProvider::WhisperLocal => build_whisper(cfg),
    }
}

/// [`build_transcriber`] wrapped in a [`ChunkedTranscriber`] using `splitter`.
pub fn build_chunked_transcriber(
    cfg: &SttConfig,
    keys: &dyn KeyResolver,
    splitter: Arc<dyn AudioSplitter>,
) -> Result<Arc<dyn Transcriber>> {
    let inner = build_transcriber(cfg, keys)?;
    Ok(Arc::new(ChunkedTranscriber::new(
        inner,
        splitter,
        cfg.chunk_opts(),
    )))
}

#[cfg(feature = "whisper")]
fn build_whisper(cfg: &SttConfig) -> Result<Arc<dyn Transcriber>> {
    let model_path = cfg
        .model_path
        .clone()
        .ok_or_else(|| SttError::Config("whisper_local needs model_path".into()))?;
    let t = crate::whisper::WhisperRsTranscriber::new(crate::whisper::WhisperOptions {
        model_path,
        threads: cfg.threads,
        default_language: cfg.language.clone(),
    })?;
    Ok(Arc::new(t))
}

#[cfg(not(feature = "whisper"))]
fn build_whisper(_cfg: &SttConfig) -> Result<Arc<dyn Transcriber>> {
    Err(SttError::Unsupported(
        "whisper_local needs the `whisper` cargo feature".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_names_and_defaults() {
        let c: SttConfig =
            serde_json::from_str(r#"{"provider":"whisper_local","model_path":"/m.bin"}"#).unwrap();
        assert_eq!(c.provider, SttProvider::WhisperLocal);
        assert_eq!(c.model, "whisper-1");
        let c: SttConfig = serde_json::from_str(r#"{"key_ref":"GROQ_API_KEY"}"#).unwrap();
        assert_eq!(c.provider, SttProvider::OpenaiCompat);
        assert_eq!(c.key_ref, Some(KeyRef::new("GROQ_API_KEY")));
    }

    #[test]
    fn builder_key_handling() {
        let none = |_: &KeyRef| None;
        let mut cfg = SttConfig::default();
        assert!(build_transcriber(&cfg, &none).is_ok()); // keyless allowed
        cfg.key_ref = Some(KeyRef::new("K"));
        assert!(matches!(
            build_transcriber(&cfg, &none),
            Err(SttError::MissingKey(_))
        ));
        let some = |_: &KeyRef| Some("sk".to_string());
        assert!(build_transcriber(&cfg, &some).is_ok());
        cfg.provider = SttProvider::WhisperLocal;
        #[cfg(not(feature = "whisper"))]
        assert!(matches!(
            build_transcriber(&cfg, &some),
            Err(SttError::Unsupported(_))
        ));
    }
}
