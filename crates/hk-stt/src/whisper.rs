//! Local whisper.cpp transcriber (cargo feature `whisper`).
//!
//! Word times come from whisper.cpp token timestamps: `max_len = 1` with
//! `split_on_word` makes every output segment one word. Input must be WAV
//! (hound); anything else should be converted upstream (hk-media).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::error::{Result, SttError};
use crate::filler::language_code;
use crate::transcriber::{TranscribeOpts, Transcriber};
use crate::transcript::{SegmentOpts, Transcript, Word};

const SAMPLE_RATE: u32 = 16_000;

/// Settings for [`WhisperRsTranscriber`].
#[derive(Debug, Clone)]
pub struct WhisperOptions {
    /// ggml model file (e.g. `ggml-base.bin`).
    pub model_path: PathBuf,
    /// CPU threads; `None` = whisper.cpp default.
    pub threads: Option<i32>,
    /// Language used when the call gives none; `None` = auto-detect.
    pub default_language: Option<String>,
    /// GPU device index; `None` = CPU only. Ignored by builds without a GPU backend.
    pub gpu: Option<i32>,
}

/// whisper.cpp through whisper-rs.
#[derive(Clone)]
pub struct WhisperRsTranscriber {
    ctx: Arc<WhisperContext>,
    opts: WhisperOptions,
}

impl std::fmt::Debug for WhisperRsTranscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhisperRsTranscriber")
            .field("opts", &self.opts)
            .finish_non_exhaustive()
    }
}

impl WhisperRsTranscriber {
    /// Load the model (blocking, can take a second or two).
    pub fn new(opts: WhisperOptions) -> Result<Self> {
        let path = opts
            .model_path
            .to_str()
            .ok_or_else(|| SttError::Config("model_path is not utf-8".into()))?;
        let mut params = WhisperContextParameters::default();
        params.use_gpu(opts.gpu.is_some());
        params.gpu_device(opts.gpu.unwrap_or(0));
        let ctx = WhisperContext::new_with_params(path, params)
            .map_err(|e| SttError::Config(format!("whisper model {path}: {e}")))?;
        Ok(Self {
            ctx: Arc::new(ctx),
            opts,
        })
    }
}

/// Decode a WAV to 16 kHz mono f32 (channel average, linear resample).
pub fn load_wav_16k_mono(path: &Path) -> Result<Vec<f32>> {
    let mut r = hound::WavReader::open(path)
        .map_err(|e| SttError::Audio(format!("{}: {e}", path.display())))?;
    let spec = r.spec();
    let ch = usize::from(spec.channels.max(1));
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<std::result::Result<_, _>>(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.saturating_sub(1))) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<std::result::Result<_, _>>()
        }
    }
    .map_err(|e| SttError::Audio(e.to_string()))?;
    let mono: Vec<f32> = interleaved
        .chunks(ch)
        .map(|f| f.iter().sum::<f32>() / f.len() as f32)
        .collect();
    if spec.sample_rate == SAMPLE_RATE || mono.is_empty() {
        return Ok(mono);
    }
    let ratio = f64::from(spec.sample_rate) / f64::from(SAMPLE_RATE);
    let n = (mono.len() as f64 / ratio) as usize;
    Ok((0..n)
        .map(|i| {
            let p = i as f64 * ratio;
            let a = p as usize;
            let b = (a + 1).min(mono.len() - 1);
            let f = (p - a as f64) as f32;
            mono[a] * (1.0 - f) + mono[b] * f
        })
        .collect())
}

#[async_trait]
impl Transcriber for WhisperRsTranscriber {
    async fn transcribe(&self, audio_path: &Path, opts: &TranscribeOpts) -> Result<Transcript> {
        let path = audio_path.to_path_buf();
        let ctx = self.ctx.clone();
        let threads = self.opts.threads;
        let language = opts
            .language
            .clone()
            .or_else(|| self.opts.default_language.clone())
            .map(|l| language_code(&l))
            .filter(|l| !l.is_empty() && l.is_ascii());
        let prompt = opts.prompt.clone();
        tokio::task::spawn_blocking(move || {
            run(&ctx, &path, threads, language.as_deref(), prompt.as_deref())
        })
        .await
        .map_err(|e| SttError::Audio(format!("whisper task: {e}")))?
    }
}

fn run(
    ctx: &WhisperContext,
    path: &Path,
    threads: Option<i32>,
    language: Option<&str>,
    prompt: Option<&str>,
) -> Result<Transcript> {
    let samples = load_wav_16k_mono(path)?;
    let duration = samples.len() as f64 / f64::from(SAMPLE_RATE);
    let mut state = ctx
        .create_state()
        .map_err(|e| SttError::Audio(format!("whisper state: {e}")))?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(language);
    if let Some(t) = threads {
        params.set_n_threads(t);
    }
    if let Some(p) = prompt.filter(|p| !p.is_empty()) {
        params.set_initial_prompt(p);
    }
    params.set_token_timestamps(true);
    params.set_split_on_word(true);
    params.set_max_len(1);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    state
        .full(params, &samples)
        .map_err(|e| SttError::Audio(format!("whisper run: {e}")))?;

    let mut words = Vec::new();
    for i in 0..state.full_n_segments() {
        let Some(seg) = state.get_segment(i) else {
            continue;
        };
        let text = seg
            .to_str_lossy()
            .map_err(|e| SttError::Audio(e.to_string()))?;
        let text = text.trim();
        if text.is_empty() || (text.starts_with('[') && text.ends_with(']')) {
            continue;
        }
        let n = seg.n_tokens();
        let conf = (n > 0).then(|| {
            let sum: f32 = (0..n)
                .filter_map(|k| seg.get_token(k))
                .map(|t| t.token_probability())
                .sum();
            (sum / n as f32).clamp(0.0, 1.0)
        });
        words.push(Word {
            confidence: conf,
            ..Word::new(
                text,
                seg.start_timestamp() as f64 / 100.0,
                seg.end_timestamp() as f64 / 100.0,
            )
        });
    }
    let lang = language
        .map(str::to_string)
        .or_else(|| whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_string));
    let mut t = Transcript {
        language: lang,
        duration,
        words,
        segments: Vec::new(),
    };
    t.fix_timings();
    t.regroup_segments(&SegmentOpts::default());
    Ok(t)
}
