//! Speech synthesis (P9.14) and image generation (P9.16) clients.
//!
//! - TTS: OpenAI-compatible `/audio/speech` (OpenAI, many local servers) or a local
//!   Piper binary.
//! - Images: OpenAI-compatible `/images/generations` or a ComfyUI server (`/prompt`,
//!   `/history`, `/view`) with a text-to-image workflow template.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum GenError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Api(String),
}

pub type Result<T> = std::result::Result<T, GenError>;

/// Speech engine config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "engine", rename_all = "snake_case")]
pub enum TtsConfig {
    OpenAi {
        base_url: String,
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default = "default_tts_model")]
        model: String,
        #[serde(default = "default_voice")]
        voice: String,
    },
    Piper {
        /// `piper` binary (PATH name or full path).
        #[serde(default = "default_piper")]
        binary: String,
        /// `.onnx` voice model.
        model: PathBuf,
    },
}

fn default_tts_model() -> String {
    "gpt-4o-mini-tts".into()
}
fn default_voice() -> String {
    "alloy".into()
}
fn default_piper() -> String {
    "piper".into()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .unwrap_or_default()
}

fn join(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

async fn check(r: reqwest::Response) -> Result<reqwest::Response> {
    if r.status().is_success() {
        Ok(r)
    } else {
        let code = r.status();
        let body = r.text().await.unwrap_or_default();
        Err(GenError::Api(format!(
            "{code}: {}",
            body.chars().take(400).collect::<String>()
        )))
    }
}

/// Synthesize `text` to `out` (wav for Piper; format from `out`'s extension for OpenAI:
/// mp3/wav/opus/flac/aac).
pub async fn speak(cfg: &TtsConfig, text: &str, out: &Path) -> Result<()> {
    if let Some(d) = out.parent() {
        tokio::fs::create_dir_all(d).await?;
    }
    match cfg {
        TtsConfig::OpenAi {
            base_url,
            api_key,
            model,
            voice,
        } => {
            let fmt = out.extension().and_then(|e| e.to_str()).unwrap_or("mp3");
            let mut req = client().post(join(base_url, "audio/speech")).json(
                &json!({"model": model, "voice": voice, "input": text, "response_format": fmt}),
            );
            if let Some(k) = api_key {
                req = req.bearer_auth(k);
            }
            let bytes = check(req.send().await?).await?.bytes().await?;
            tokio::fs::write(out, &bytes).await?;
            Ok(())
        }
        TtsConfig::Piper { binary, model } => {
            use tokio::io::AsyncWriteExt;
            let mut child = tokio::process::Command::new(binary)
                .arg("--model")
                .arg(model)
                .arg("--output_file")
                .arg(out)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(text.as_bytes()).await?;
            }
            let o = child.wait_with_output().await?;
            if o.status.success() {
                Ok(())
            } else {
                Err(GenError::Api(
                    String::from_utf8_lossy(&o.stderr)
                        .chars()
                        .take(400)
                        .collect(),
                ))
            }
        }
    }
}

/// Image engine config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "engine", rename_all = "snake_case")]
pub enum ImageConfig {
    OpenAi {
        base_url: String,
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default = "default_image_model")]
        model: String,
    },
    ComfyUi {
        /// e.g. `http://127.0.0.1:8188`.
        base_url: String,
        /// API-format workflow; `{{prompt}}`, `{{negative}}`, `{{width}}`, `{{height}}`,
        /// `{{seed}}` placeholders in string values. Default: a basic SD checkpoint graph.
        #[serde(default)]
        workflow: Option<Value>,
        #[serde(default = "default_ckpt")]
        checkpoint: String,
    },
}

fn default_image_model() -> String {
    "gpt-image-1".into()
}
fn default_ckpt() -> String {
    "sd_xl_base_1.0.safetensors".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageRequest {
    pub prompt: String,
    #[serde(default)]
    pub negative: Option<String>,
    #[serde(default = "default_side")]
    pub width: u32,
    #[serde(default = "default_side")]
    pub height: u32,
    #[serde(default)]
    pub seed: Option<u64>,
}

fn default_side() -> u32 {
    1024
}

/// Generate one image into `out` (PNG).
pub async fn generate_image(cfg: &ImageConfig, req: &ImageRequest, out: &Path) -> Result<()> {
    if let Some(d) = out.parent() {
        tokio::fs::create_dir_all(d).await?;
    }
    match cfg {
        ImageConfig::OpenAi {
            base_url,
            api_key,
            model,
        } => {
            let mut r = client()
                .post(join(base_url, "images/generations"))
                .json(&json!({
                    "model": model, "prompt": req.prompt, "n": 1,
                    "size": format!("{}x{}", req.width, req.height),
                }));
            if let Some(k) = api_key {
                r = r.bearer_auth(k);
            }
            let v: Value = check(r.send().await?).await?.json().await?;
            let item = &v["data"][0];
            let bytes = if let Some(b64) = item["b64_json"].as_str() {
                base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| GenError::Api(format!("bad base64: {e}")))?
            } else if let Some(url) = item["url"].as_str() {
                check(client().get(url).send().await?)
                    .await?
                    .bytes()
                    .await?
                    .to_vec()
            } else {
                return Err(GenError::Api("no image in response".into()));
            };
            tokio::fs::write(out, bytes).await?;
            Ok(())
        }
        ImageConfig::ComfyUi {
            base_url,
            workflow,
            checkpoint,
        } => {
            let graph = fill(
                workflow
                    .clone()
                    .unwrap_or_else(|| default_comfy(checkpoint)),
                req,
            );
            let c = client();
            let v: Value = check(
                c.post(join(base_url, "prompt"))
                    .json(&json!({"prompt": graph}))
                    .send()
                    .await?,
            )
            .await?
            .json()
            .await?;
            let id = v["prompt_id"]
                .as_str()
                .ok_or_else(|| GenError::Api("no prompt_id".into()))?
                .to_string();
            for _ in 0..600 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let h: Value = check(
                    c.get(join(base_url, &format!("history/{id}")))
                        .send()
                        .await?,
                )
                .await?
                .json()
                .await?;
                let Some(outputs) = h[&id]["outputs"].as_object() else {
                    continue;
                };
                for o in outputs.values() {
                    if let Some(img) = o["images"].get(0) {
                        let url = format!(
                            "{}?filename={}&subfolder={}&type={}",
                            join(base_url, "view"),
                            img["filename"].as_str().unwrap_or_default(),
                            img["subfolder"].as_str().unwrap_or_default(),
                            img["type"].as_str().unwrap_or("output")
                        );
                        let bytes = check(c.get(url).send().await?).await?.bytes().await?;
                        tokio::fs::write(out, &bytes).await?;
                        return Ok(());
                    }
                }
            }
            Err(GenError::Api("ComfyUI timed out".into()))
        }
    }
}

/// Replace `{{…}}` placeholders in every string of a workflow.
pub fn fill(v: Value, r: &ImageRequest) -> Value {
    match v {
        Value::String(s) => {
            let whole = |k: &str| s == format!("{{{{{k}}}}}");
            if whole("width") {
                return json!(r.width);
            }
            if whole("height") {
                return json!(r.height);
            }
            if whole("seed") {
                return json!(r.seed.unwrap_or(42));
            }
            Value::String(
                s.replace("{{prompt}}", &r.prompt)
                    .replace("{{negative}}", r.negative.as_deref().unwrap_or("")),
            )
        }
        Value::Array(a) => Value::Array(a.into_iter().map(|x| fill(x, r)).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, fill(x, r))).collect()),
        other => other,
    }
}

/// Minimal SD text-to-image graph (API format).
pub fn default_comfy(ckpt: &str) -> Value {
    json!({
        "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": ckpt}},
        "5": {"class_type": "EmptyLatentImage", "inputs": {"width": "{{width}}", "height": "{{height}}", "batch_size": 1}},
        "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{prompt}}", "clip": ["4", 1]}},
        "7": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{negative}}", "clip": ["4", 1]}},
        "3": {"class_type": "KSampler", "inputs": {"seed": "{{seed}}", "steps": 25, "cfg": 7, "sampler_name": "euler", "scheduler": "normal", "denoise": 1, "model": ["4", 0], "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["5", 0]}},
        "8": {"class_type": "VAEDecode", "inputs": {"samples": ["3", 0], "vae": ["4", 2]}},
        "9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "hokkabaz", "images": ["8", 0]}}
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_typed_placeholders() {
        let r = ImageRequest {
            prompt: "a cat".into(),
            negative: None,
            width: 512,
            height: 768,
            seed: Some(7),
        };
        let g = fill(default_comfy("x.safetensors"), &r);
        assert_eq!(g["5"]["inputs"]["width"], 512);
        assert_eq!(g["3"]["inputs"]["seed"], 7);
        assert_eq!(g["6"]["inputs"]["text"], "a cat");
    }
}

/// Embedding engine for knowledge-base search (hybrid with BM25).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "engine", rename_all = "snake_case")]
pub enum EmbedConfig {
    /// `/embeddings` (OpenAI, many local servers).
    OpenAi {
        base_url: String,
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default = "default_embed_model")]
        model: String,
    },
    /// Ollama `/api/embed`.
    Ollama {
        #[serde(default = "default_ollama_url")]
        base_url: String,
        #[serde(default = "default_ollama_embed")]
        model: String,
    },
}

fn default_embed_model() -> String {
    "text-embedding-3-small".into()
}
fn default_ollama_url() -> String {
    "http://127.0.0.1:11434".into()
}
fn default_ollama_embed() -> String {
    "nomic-embed-text".into()
}

/// Embed `texts` (one vector per text, same order).
pub async fn embed(cfg: &EmbedConfig, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let vecs = |v: &Value, key: &str| -> Vec<Vec<f32>> {
        v[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|e| {
                        let arr = if key == "data" { &e["embedding"] } else { e };
                        arr.as_array()
                            .map(|x| {
                                x.iter()
                                    .filter_map(Value::as_f64)
                                    .map(|f| f as f32)
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    match cfg {
        EmbedConfig::OpenAi {
            base_url,
            api_key,
            model,
        } => {
            let mut r = client()
                .post(join(base_url, "embeddings"))
                .json(&json!({"model": model, "input": texts}));
            if let Some(k) = api_key {
                r = r.bearer_auth(k);
            }
            let v: Value = check(r.send().await?).await?.json().await?;
            Ok(vecs(&v, "data"))
        }
        EmbedConfig::Ollama { base_url, model } => {
            let v: Value = check(
                client()
                    .post(join(base_url, "api/embed"))
                    .json(&json!({"model": model, "input": texts}))
                    .send()
                    .await?,
            )
            .await?
            .json()
            .await?;
            Ok(vecs(&v, "embeddings"))
        }
    }
}
