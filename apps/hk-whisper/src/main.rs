//! `hk-whisper`: transcribe one audio file with whisper.cpp and print the word-level
//! [`hk_stt::Transcript`] as JSON on stdout. Exists so the app and `hokkabazd` (which link
//! llama.cpp) never link a second copy of ggml: see `hk_stt::external`.
//!
//! `hk-whisper --model M.bin --audio A.wav [--lang xx] [--threads N] [--prompt TEXT] [--gpu N | --cpu]`
//! `hk-whisper --version`, `hk-whisper --info` (JSON: version, compiled-in GPU backend)

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use hk_stt::whisper::{WhisperOptions, WhisperRsTranscriber};
use hk_stt::{TranscribeOpts, Transcriber};

#[derive(Default)]
struct Args {
    model: Option<PathBuf>,
    audio: Option<PathBuf>,
    lang: Option<String>,
    threads: Option<i32>,
    prompt: Option<String>,
    gpu: Option<i32>,
}

/// GPU backend compiled into this helper, if any.
const GPU_BACKEND: Option<&str> = if cfg!(feature = "vulkan") {
    Some("vulkan")
} else if cfg!(feature = "cuda") {
    Some("cuda")
} else if cfg!(feature = "metal") {
    Some("metal")
} else {
    None
};

enum Cmd {
    Version,
    Info,
    Run(Args),
}

fn parse() -> Result<Cmd> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = |name: &str| it.next().with_context(|| format!("{name} needs a value"));
        match k.as_str() {
            "--version" | "-V" => return Ok(Cmd::Version),
            "--info" => return Ok(Cmd::Info),
            "--gpu" => {
                let v = val("--gpu")?;
                // `auto`: the discrete GPU with the most memory (never an iGPU over NVIDIA/AMD).
                a.gpu = Some(if v == "auto" {
                    devices::best().unwrap_or(0)
                } else {
                    v.parse().context("--gpu")?
                });
            }
            "--cpu" => a.gpu = None,
            "--model" => a.model = Some(val("--model")?.into()),
            "--audio" => a.audio = Some(val("--audio")?.into()),
            "--lang" => a.lang = Some(val("--lang")?).filter(|l| !l.is_empty() && l != "auto"),
            "--threads" => a.threads = Some(val("--threads")?.parse().context("--threads")?),
            "--prompt" => a.prompt = Some(val("--prompt")?),
            other => bail!("unknown argument `{other}`"),
        }
    }
    Ok(Cmd::Run(a))
}

#[allow(clippy::print_stdout)]
fn run() -> Result<()> {
    let a = match parse()? {
        Cmd::Version => {
            println!("hk-whisper {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Cmd::Info => {
            let info = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "gpu": GPU_BACKEND,
                "devices": if GPU_BACKEND.is_some() { devices::list() } else { Vec::new() },
                "auto_device": devices::best(),
            });
            println!("{info}");
            return Ok(());
        }
        Cmd::Run(a) => a,
    };
    let model = a.model.context("--model is required")?;
    let audio = a.audio.context("--audio is required")?;
    let t = WhisperRsTranscriber::new(WhisperOptions {
        model_path: model,
        threads: a.threads,
        default_language: a.lang.clone(),
        gpu: a.gpu.filter(|_| GPU_BACKEND.is_some()),
    })?;
    let rt = tokio::runtime::Builder::new_current_thread().build()?;
    let tr = rt.block_on(t.transcribe(
        &audio,
        &TranscribeOpts {
            language: a.lang,
            prompt: a.prompt,
            word_timestamps: true,
        },
    ))?;
    println!("{}", serde_json::to_string(&tr)?);
    Ok(())
}

#[allow(clippy::print_stderr)]
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// ggml GPU devices as whisper.cpp counts them for `gpu_device` (GPU and iGPU, in order).
#[allow(unsafe_code)]
mod devices {
    use std::ffi::CStr;

    use whisper_rs_sys as sys;

    pub(crate) fn list() -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        // SAFETY: plain ggml registry queries; props strings are owned by the registry.
        unsafe {
            let mut pos = 0;
            for i in 0..sys::ggml_backend_dev_count() {
                let dev = sys::ggml_backend_dev_get(i);
                let mut p: sys::ggml_backend_dev_props = std::mem::zeroed();
                sys::ggml_backend_dev_get_props(dev, &raw mut p);
                let integrated =
                    p.type_ == sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU;
                if p.type_ != sys::ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU && !integrated
                {
                    continue;
                }
                let s = |c: *const std::os::raw::c_char| {
                    if c.is_null() {
                        String::new()
                    } else {
                        CStr::from_ptr(c).to_string_lossy().into_owned()
                    }
                };
                out.push(serde_json::json!({
                    "device": pos,
                    "name": s(p.name),
                    "description": s(p.description),
                    "memory_total": p.memory_total,
                    "integrated": integrated,
                }));
                pos += 1;
            }
        }
        out
    }

    /// Discrete GPU with the most memory, else the first GPU.
    pub(crate) fn best() -> Option<i32> {
        let all = list();
        all.iter()
            .filter(|d| d["integrated"] == false)
            .max_by_key(|d| d["memory_total"].as_u64().unwrap_or(0))
            .or_else(|| all.first())
            .and_then(|d| d["device"].as_i64())
            .map(|d| d as i32)
    }
}
