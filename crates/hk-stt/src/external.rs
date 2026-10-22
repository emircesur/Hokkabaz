//! Local whisper.cpp through the `hk-whisper` helper process.
//!
//! whisper.cpp and llama.cpp each bundle their own copy of ggml. Linked into one binary the two
//! collide (duplicate or silently merged `ggml_*` symbols), so the engine that transcribes lives
//! in its own executable: this crate spawns it per audio chunk and reads one JSON [`Transcript`]
//! from its stdout. Same words/timestamps as the in-process engine (cargo feature `whisper`).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::error::{Result, SttError};
use crate::transcriber::{TranscribeOpts, Transcriber};
use crate::transcript::Transcript;

/// Executable name of the helper (`.exe` on Windows).
pub const HELPER: &str = if cfg!(windows) {
    "hk-whisper.exe"
} else {
    "hk-whisper"
};

/// `HK_WHISPER_BIN`, then next to the running executable, then `PATH`.
pub fn find_helper() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HK_WHISPER_BIN").map(PathBuf::from)
        && p.is_file()
    {
        return Some(p);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for d in [
            dir.to_path_buf(),
            dir.join("bin"),
            dir.join("../lib/hokkabaz"),
        ] {
            let p = d.join(HELPER);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(HELPER))
            .find(|p| p.is_file())
    })
}

/// Settings for [`ExternalWhisper`].
#[derive(Debug, Clone)]
pub struct ExternalOptions {
    /// The `hk-whisper` executable.
    pub helper: PathBuf,
    /// ggml model file (e.g. `ggml-base-q5_1.bin`).
    pub model_path: PathBuf,
    /// CPU threads; `None` = whisper.cpp default.
    pub threads: Option<i32>,
    /// Language used when the call gives none; `None` = auto-detect.
    pub default_language: Option<String>,
}

/// Transcriber backed by the `hk-whisper` helper.
#[derive(Debug, Clone)]
pub struct ExternalWhisper {
    opts: ExternalOptions,
}

impl ExternalWhisper {
    pub fn new(opts: ExternalOptions) -> Result<Self> {
        if !opts.helper.is_file() {
            return Err(SttError::Config(format!(
                "{} not found: {}",
                HELPER,
                opts.helper.display()
            )));
        }
        if !opts.model_path.is_file() {
            return Err(SttError::Config(format!(
                "whisper model not found: {}",
                opts.model_path.display()
            )));
        }
        Ok(Self { opts })
    }
}

#[async_trait]
impl Transcriber for ExternalWhisper {
    async fn transcribe(&self, audio_path: &Path, opts: &TranscribeOpts) -> Result<Transcript> {
        let mut cmd = Command::new(&self.opts.helper);
        cmd.arg("--model")
            .arg(&self.opts.model_path)
            .arg("--audio")
            .arg(audio_path);
        if let Some(l) = opts
            .language
            .as_ref()
            .or(self.opts.default_language.as_ref())
        {
            cmd.arg("--lang").arg(l);
        }
        if let Some(t) = self.opts.threads {
            cmd.arg("--threads").arg(t.to_string());
        }
        if let Some(p) = opts.prompt.as_ref().filter(|p| !p.is_empty()) {
            cmd.arg("--prompt").arg(p);
        }
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A cancelled import must not leave a CPU-bound child behind.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SttError::Audio(format!("cannot start {HELPER}: {e}")))?;
        let mut out = child.stdout.take().expect("piped");
        let mut err = child.stderr.take().expect("piped");
        let (stdout, stderr) = tokio::join!(
            async {
                let mut b = Vec::new();
                out.read_to_end(&mut b).await.map(|_| b)
            },
            async {
                let mut b = Vec::new();
                err.read_to_end(&mut b).await.map(|_| b)
            }
        );
        let status = child
            .wait()
            .await
            .map_err(|e| SttError::Audio(format!("{HELPER}: {e}")))?;
        if !status.success() {
            let msg = String::from_utf8_lossy(&stderr.unwrap_or_default()).into_owned();
            let tail: String = msg
                .lines()
                .rev()
                .find(|l| l.trim_start().starts_with("error:"))
                .or_else(|| msg.lines().rev().find(|l| !l.trim().is_empty()))
                .unwrap_or("no output")
                .chars()
                .take(300)
                .collect();
            return Err(SttError::Audio(format!(
                "{HELPER} failed ({status}): {tail}"
            )));
        }
        serde_json::from_slice(&stdout.map_err(|e| SttError::Audio(e.to_string()))?)
            .map_err(|e| SttError::Parse(format!("{HELPER} output: {e}")))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A fake helper that answers with a fixed transcript and echoes its language flag.
    fn fake(dir: &Path, script: &str) -> PathBuf {
        let p = dir.join(HELPER);
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn opts(helper: PathBuf, dir: &Path) -> ExternalOptions {
        let model = dir.join("m.bin");
        std::fs::write(&model, b"x").unwrap();
        ExternalOptions {
            helper,
            model_path: model,
            threads: Some(2),
            default_language: None,
        }
    }

    #[tokio::test]
    async fn parses_helper_json() {
        let d = tempfile::tempdir().unwrap();
        let h = fake(
            d.path(),
            r#"echo '{"language":"tr","duration":1.0,"words":[{"text":"merhaba","start":0.1,"end":0.5}],"segments":[]}'"#,
        );
        let t = ExternalWhisper::new(opts(h, d.path())).unwrap();
        let tr = t
            .transcribe(Path::new("a.wav"), &TranscribeOpts::default())
            .await
            .unwrap();
        assert_eq!(tr.language.as_deref(), Some("tr"));
        assert_eq!(tr.words[0].text, "merhaba");
    }

    #[tokio::test]
    async fn surfaces_helper_error() {
        let d = tempfile::tempdir().unwrap();
        let h = fake(d.path(), "echo 'error: bad model' >&2; exit 1");
        let t = ExternalWhisper::new(opts(h, d.path())).unwrap();
        let e = t
            .transcribe(Path::new("a.wav"), &TranscribeOpts::default())
            .await
            .unwrap_err();
        assert!(e.to_string().contains("bad model"), "{e}");
    }

    #[test]
    fn missing_model_is_a_config_error() {
        let d = tempfile::tempdir().unwrap();
        let h = fake(d.path(), "true");
        let mut o = opts(h, d.path());
        o.model_path = d.path().join("nope.bin");
        assert!(matches!(ExternalWhisper::new(o), Err(SttError::Config(_))));
    }
}
