//! `POST {base_url}/audio/transcriptions` (OpenAI, Groq, faster-whisper-server,
//! whisper.cpp server, ...). Multipart body is built by hand: tiny, retry-safe
//! (one `Bytes`, cloned per attempt) and avoids reqwest's `multipart` + mime deps.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use serde_json::Value;

use crate::error::{Result, SttError};
use crate::filler::language_code;
use crate::transcriber::{TranscribeOpts, Transcriber};
use crate::transcript::{Segment, SegmentOpts, Transcript, Word, WordFlags};

/// OpenAI's hard request limit; also what Groq's free tier documents.
pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 25 * 1024 * 1024;

/// Tuning for [`OpenAiCompatTranscriber`].
#[derive(Debug, Clone)]
pub struct OpenAiCompatOptions {
    /// Retries after the first attempt on 429/5xx/transport errors.
    pub max_retries: u32,
    /// First backoff; doubles each retry (capped at 30 s). `Retry-After` wins.
    pub backoff_base: Duration,
    /// Whole-request timeout.
    pub timeout: Duration,
    /// Refuse bigger files before uploading.
    pub max_upload_bytes: u64,
    /// Extra static headers.
    pub headers: BTreeMap<String, String>,
    /// Language used when the call gives none.
    pub default_language: Option<String>,
}

impl Default for OpenAiCompatOptions {
    fn default() -> Self {
        Self {
            max_retries: 3,
            backoff_base: Duration::from_millis(800),
            timeout: Duration::from_secs(600),
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            headers: BTreeMap::new(),
            default_language: None,
        }
    }
}

/// Transcriber over an OpenAI-compatible HTTP API.
#[derive(Clone)]
pub struct OpenAiCompatTranscriber {
    client: reqwest::Client,
    url: String,
    model: String,
    key: Option<String>,
    opts: OpenAiCompatOptions,
}

impl std::fmt::Debug for OpenAiCompatTranscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatTranscriber")
            .field("url", &self.url)
            .field("model", &self.model)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl OpenAiCompatTranscriber {
    /// `base_url` is the API root incl. version (`https://api.openai.com/v1`).
    pub fn new(
        base_url: &str,
        model: impl Into<String>,
        api_key: Option<String>,
        opts: OpenAiCompatOptions,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(opts.timeout)
            .build()
            .map_err(|e| SttError::Config(format!("http client: {e}")))?;
        Ok(Self {
            client,
            url: format!("{}/audio/transcriptions", base_url.trim_end_matches('/')),
            model: model.into(),
            key: api_key.filter(|k| !k.is_empty()),
            opts,
        })
    }

    async fn post(&self, boundary: &str, body: Bytes) -> Result<String> {
        let mut attempt = 0u32;
        loop {
            let mut req = self
                .client
                .post(&self.url)
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(body.clone());
            if let Some(k) = &self.key {
                req = req.bearer_auth(k);
            }
            for (k, v) in &self.opts.headers {
                req = req.header(k, v);
            }
            let (retry_after, err) = match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .map(|s| Duration::from_secs(s.min(60)));
                    let text = resp
                        .text()
                        .await
                        .map_err(|e| SttError::Network(e.to_string()))?;
                    if status.is_success() {
                        return Ok(text);
                    }
                    let code = status.as_u16();
                    if code == 413 {
                        return Err(SttError::TooLarge {
                            size: body.len() as u64,
                            limit: self.opts.max_upload_bytes,
                        });
                    }
                    let e = SttError::Http {
                        status: code,
                        body: truncate(&text, 500),
                    };
                    if code != 429 && !status.is_server_error() {
                        return Err(e);
                    }
                    (retry_after, e)
                }
                Err(e) if e.is_timeout() || e.is_connect() || e.is_request() => {
                    (None, SttError::Network(e.without_url().to_string()))
                }
                Err(e) => return Err(SttError::Network(e.without_url().to_string())),
            };
            if attempt >= self.opts.max_retries {
                return Err(err);
            }
            let wait = retry_after.unwrap_or_else(|| {
                (self.opts.backoff_base * 2u32.saturating_pow(attempt)).min(Duration::from_secs(30))
            });
            tracing::warn!(attempt, ?wait, error = %err, "transcription retry");
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }
}

#[async_trait]
impl Transcriber for OpenAiCompatTranscriber {
    async fn transcribe(&self, audio_path: &Path, opts: &TranscribeOpts) -> Result<Transcript> {
        let io = |source| SttError::Io {
            path: audio_path.to_path_buf(),
            source,
        };
        let size = tokio::fs::metadata(audio_path).await.map_err(io)?.len();
        if size > self.opts.max_upload_bytes {
            return Err(SttError::TooLarge {
                size,
                limit: self.opts.max_upload_bytes,
            });
        }
        let data = tokio::fs::read(audio_path).await.map_err(io)?;
        let language = opts
            .language
            .clone()
            .or_else(|| self.opts.default_language.clone());
        let filename = audio_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio");
        let (boundary, body) = multipart_body(
            filename,
            content_type_for(audio_path),
            &data,
            &self.model,
            opts,
            language.as_deref(),
        );
        let text = self.post(&boundary, body).await?;
        parse_verbose_json(&text, language.as_deref())
    }

    fn max_upload_bytes(&self) -> Option<u64> {
        Some(self.opts.max_upload_bytes)
    }
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn content_type_for(p: &Path) -> &'static str {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp3" | "mpeg" | "mpga") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("m4a" | "mp4") => "audio/mp4",
        Some("ogg" | "opus" | "oga") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("webm") => "audio/webm",
        _ => "application/octet-stream",
    }
}

static BOUNDARY_SEQ: AtomicU64 = AtomicU64::new(0);

/// Build the multipart body. Boundary is unique per call and checked absent
/// from the payload.
fn multipart_body(
    filename: &str,
    content_type: &str,
    data: &[u8],
    model: &str,
    opts: &TranscribeOpts,
    language: Option<&str>,
) -> (String, Bytes) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut boundary = format!(
        "hkstt{nanos:x}{:x}",
        BOUNDARY_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    while data
        .windows(boundary.len())
        .any(|w| w == boundary.as_bytes())
    {
        boundary.push('x');
    }
    let mut body: Vec<u8> = Vec::with_capacity(data.len() + 1024);
    let mut field = |name: &str, value: &str| {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    };
    field("model", model);
    field("response_format", "verbose_json");
    if opts.word_timestamps {
        field("timestamp_granularities[]", "word");
    }
    field("timestamp_granularities[]", "segment");
    if let Some(l) = language {
        field("language", &language_code(l));
    }
    if let Some(p) = opts.prompt.as_deref().filter(|p| !p.is_empty()) {
        field("prompt", p);
    }
    let safe_name: String = filename
        .chars()
        .filter(|c| !matches!(c, '"' | '\r' | '\n' | '\\'))
        .collect();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{safe_name}\"\r\nContent-Type: {content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (boundary, Bytes::from(body))
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn parse_word(v: &Value, seg_conf: Option<f32>) -> Option<Word> {
    let text = v.get("word").or_else(|| v.get("text"))?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    let start = num(v.get("start"))?;
    let end = num(v.get("end")).unwrap_or(start);
    let conf = num(v.get("probability").or_else(|| v.get("confidence")))
        .map(|c| c as f32)
        .or(seg_conf);
    Some(Word {
        confidence: conf,
        ..Word::new(text, start, end)
    })
}

/// Spread `text` over `[start, end]` proportionally to token length.
fn interpolate(text: &str, start: f64, end: f64, conf: Option<f32>) -> Vec<Word> {
    let toks: Vec<&str> = text.split_whitespace().collect();
    if toks.is_empty() {
        return Vec::new();
    }
    let end = if end > start {
        end
    } else {
        start + 0.35 * toks.len() as f64
    };
    let weights: Vec<f64> = toks
        .iter()
        .map(|t| t.chars().count() as f64 + 1.0)
        .collect();
    let total: f64 = weights.iter().sum();
    let mut t = start;
    toks.iter()
        .zip(weights)
        .map(|(tok, w)| {
            let e = t + (end - start) * w / total;
            let word = Word {
                confidence: conf,
                flags: WordFlags {
                    estimated: true,
                    ..Default::default()
                },
                ..Word::new(*tok, t, e)
            };
            t = e;
            word
        })
        .collect()
}

/// Parse an OpenAI-style `verbose_json` body (also what Groq, faster-whisper
/// servers and whisper.cpp emit). Words may be top-level or nested per
/// segment; segment-only responses get interpolated (flagged `estimated`)
/// words; plain `{text}` responses become one segment.
pub fn parse_verbose_json(body: &str, fallback_language: Option<&str>) -> Result<Transcript> {
    let v: Value =
        serde_json::from_str(body).map_err(|e| SttError::Parse(format!("not json: {e}")))?;
    if !v.is_object() {
        return Err(SttError::Parse("expected json object".into()));
    }
    let language = v
        .get("language")
        .and_then(Value::as_str)
        .filter(|l| !l.is_empty())
        .or(fallback_language)
        .map(language_code)
        .filter(|l| !l.is_empty());
    let mut duration = num(v.get("duration")).unwrap_or(0.0);

    let mut segments: Vec<Segment> = Vec::new();
    let mut seg_words: Vec<Word> = Vec::new();
    let mut nested = false;
    if let Some(arr) = v.get("segments").and_then(Value::as_array) {
        for s in arr {
            let text = s
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let start = num(s.get("start")).unwrap_or(0.0);
            let end = num(s.get("end")).unwrap_or(start);
            let conf = num(s.get("avg_logprob")).map(|lp| lp.exp().clamp(0.0, 1.0) as f32);
            let mut words: Vec<Word> = s
                .get("words")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|w| parse_word(w, conf)).collect())
                .unwrap_or_default();
            if words.is_empty() {
                words = interpolate(&text, start, end, conf);
            } else {
                nested = true;
            }
            seg_words.extend(words);
            segments.push(Segment {
                text,
                start,
                end,
                word_range: 0..0,
            });
        }
    }

    let top: Vec<Word> = v
        .get("words")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|w| parse_word(w, None)).collect())
        .unwrap_or_default();
    let mut words = if !top.is_empty() {
        top
    } else if !seg_words.is_empty() || nested {
        seg_words
    } else if segments.is_empty() {
        // Server ignored verbose_json: only `text`.
        let text = v.get("text").and_then(Value::as_str).unwrap_or("").trim();
        let w = interpolate(text, 0.0, duration, None);
        if !text.is_empty() {
            let end = w.last().map_or(0.0, |w| w.end);
            segments.push(Segment {
                text: text.to_string(),
                start: 0.0,
                end,
                word_range: 0..0,
            });
        }
        w
    } else {
        Vec::new()
    };
    // Top-level words carry no segment confidence; borrow it by time when missing.
    if words.iter().any(|w| w.confidence.is_none()) && !segments.is_empty() {
        let confs: Vec<Option<f32>> = v
            .get("segments")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|s| num(s.get("avg_logprob")).map(|lp| lp.exp().clamp(0.0, 1.0) as f32))
                    .collect()
            })
            .unwrap_or_default();
        for w in words.iter_mut().filter(|w| w.confidence.is_none()) {
            let m = w.mid();
            if let Some(i) = segments
                .iter()
                .position(|s| m >= s.start && m < s.end.max(s.start + 1e-6))
            {
                w.confidence = confs.get(i).copied().flatten();
            }
        }
    }

    let mut t = Transcript {
        language,
        duration,
        words: Vec::new(),
        segments,
    };
    duration = duration.max(words.last().map_or(0.0, |w| w.end));
    t.duration = duration;
    t.words = std::mem::take(&mut words);
    t.fix_timings();
    if t.segments.is_empty() {
        t.regroup_segments(&SegmentOpts::default());
    } else {
        t.link_segments();
    }
    Ok(t)
}
