//! Local whisper.cpp through the `hk-whisper` helper process.
//!
//! whisper.cpp and llama.cpp each bundle their own copy of ggml. Linked into one binary the two
//! collide (duplicate or silently merged `ggml_*` symbols), so the engine that transcribes lives
//! in its own executable: this crate spawns it per audio chunk and reads one JSON [`Transcript`]
//! from its stdout. Same words/timestamps as the in-process engine (cargo feature `whisper`).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// GPU backend compiled into the helper (`vulkan`, `cuda`, `metal`), from `--info`; cached.
pub fn helper_gpu() -> Option<String> {
    static GPU: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    GPU.get_or_init(|| {
        let helper = find_helper()?;
        let mut cmd = std::process::Command::new(helper);
        cmd.arg("--info").stdin(Stdio::null()).stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            cmd.creation_flags(0x0800_0000);
        }
        let out = cmd.output().ok()?;
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        v["gpu"].as_str().map(str::to_owned)
    })
    .clone()
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
    /// GPU device; `None` = CPU. A failed GPU run is retried on the CPU, which then sticks.
    pub gpu: Option<i32>,
}

/// Transcriber backed by the `hk-whisper` helper.
#[derive(Debug, Clone)]
pub struct ExternalWhisper {
    opts: ExternalOptions,
    gpu_failed: Arc<AtomicBool>,
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
        Ok(Self {
            opts,
            gpu_failed: Arc::default(),
        })
    }
}

#[async_trait]
impl Transcriber for ExternalWhisper {
    async fn transcribe(&self, audio_path: &Path, opts: &TranscribeOpts) -> Result<Transcript> {
        let gpu = self
            .opts
            .gpu
            .filter(|_| !self.gpu_failed.load(Ordering::Relaxed));
        match self.run(audio_path, opts, gpu).await {
            Err(e) if gpu.is_some() => {
                tracing::warn!(error = %e, "whisper on the GPU failed; using the CPU");
                self.gpu_failed.store(true, Ordering::Relaxed);
                self.run(audio_path, opts, None).await
            }
            r => r,
        }
    }
}

impl ExternalWhisper {
    async fn run(
        &self,
        audio_path: &Path,
        opts: &TranscribeOpts,
        gpu: Option<i32>,
    ) -> Result<Transcript> {
        let mut cmd = Command::new(&self.opts.helper);
        match gpu {
            Some(d) => cmd.arg("--gpu").arg(d.to_string()),
            None => cmd.arg("--cpu"),
        };
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
        // No console window flashing up per chunk on Windows.
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
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
            if let Some(why) = crash_reason(&status) {
                return Err(SttError::Audio(format!("{HELPER} crashed: {why}")));
            }
            return Err(SttError::Audio(format!(
                "{HELPER} failed ({status}): {tail}"
            )));
        }
        serde_json::from_slice(&stdout.map_err(|e| SttError::Audio(e.to_string()))?)
            .map_err(|e| SttError::Parse(format!("{HELPER} output: {e}")))
    }
}

/// Plain words for the crashes users actually hit (they print nothing on stderr).
fn crash_reason(status: &std::process::ExitStatus) -> Option<&'static str> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if status.signal() == Some(4) {
            return Some(
                "this CPU lacks instructions the helper was built for (illegal instruction)",
            );
        }
    }
    // NTSTATUS codes come back as the exit code.
    match status.code().map(|c| c as u32) {
        Some(0xC000_001D) => {
            Some("this CPU lacks instructions the helper was built for (illegal instruction)")
        }
        Some(0xC000_0135) => Some(
            "a DLL it needs is missing; install the Microsoft Visual C++ Redistributable (x64)",
        ),
        Some(0xC000_0005) => {
            Some("access violation (the model file may be damaged; download it again)")
        }
        _ => None,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Writing a script while another test forks leaks the write fd into that child, and exec
    /// then fails with ETXTBSY: tests that write and spawn helpers run one at a time.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
            gpu: None,
        }
    }

    #[tokio::test]
    async fn parses_helper_json() {
        let _serial = SERIAL.lock().await;
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
        let _serial = SERIAL.lock().await;
        let d = tempfile::tempdir().unwrap();
        let h = fake(d.path(), "echo 'error: bad model' >&2; exit 1");
        let t = ExternalWhisper::new(opts(h, d.path())).unwrap();
        let e = t
            .transcribe(Path::new("a.wav"), &TranscribeOpts::default())
            .await
            .unwrap_err();
        assert!(e.to_string().contains("bad model"), "{e}");
    }

    #[tokio::test]
    async fn gpu_failure_falls_back_to_cpu() {
        let _serial = SERIAL.lock().await;
        let d = tempfile::tempdir().unwrap();
        let h = fake(
            d.path(),
            r#"[ "$1" = "--gpu" ] && { echo 'error: no vulkan device' >&2; exit 1; }
echo '{"language":"en","duration":1.0,"words":[],"segments":[]}'"#,
        );
        let mut o = opts(h, d.path());
        o.gpu = Some(0);
        let t = ExternalWhisper::new(o).unwrap();
        for _ in 0..2 {
            let tr = t
                .transcribe(Path::new("a.wav"), &TranscribeOpts::default())
                .await
                .unwrap();
            assert_eq!(tr.language.as_deref(), Some("en"));
        }
        assert!(t.gpu_failed.load(Ordering::Relaxed));
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
