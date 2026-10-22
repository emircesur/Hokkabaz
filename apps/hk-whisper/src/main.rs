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
            "--gpu" => a.gpu = Some(val("--gpu")?.parse().context("--gpu")?),
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
            let info =
                serde_json::json!({"version": env!("CARGO_PKG_VERSION"), "gpu": GPU_BACKEND});
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
