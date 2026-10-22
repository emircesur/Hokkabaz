//! One-shot process runner. argv is always a vec, never a shell string.
use std::path::Path;

use crate::error::Result;

#[derive(Debug)]
pub(crate) struct Captured {
    pub stdout: String,
    pub stderr: String,
}

/// Keep only the last `max` lines of an error stream; ffmpeg prints the cause last.
pub(crate) fn tail_lines(s: &str, max: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(max)..].join("\n")
}

#[cfg(not(target_os = "android"))]
pub(crate) async fn run_capture(bin: &Path, args: &[String]) -> Result<Captured> {
    use std::process::Stdio;

    use crate::error::MediaError;

    tracing::debug!(bin = %bin.display(), ?args, "spawn");
    let out = tokio::process::Command::new(crate::tools::resolve_program(bin))
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| spawn_err(bin, e))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Err(MediaError::Failed {
            bin: bin.display().to_string(),
            code: out.status.code(),
            stderr_tail: tail_lines(&stderr, 30),
        });
    }
    Ok(Captured { stdout, stderr })
}

#[cfg(not(target_os = "android"))]
pub(crate) fn spawn_err(bin: &Path, e: std::io::Error) -> crate::error::MediaError {
    if e.kind() == std::io::ErrorKind::NotFound {
        crate::error::MediaError::BinaryNotFound(bin.to_path_buf())
    } else {
        crate::error::MediaError::Io(e)
    }
}

#[cfg(target_os = "android")]
pub(crate) async fn run_capture(_bin: &Path, _args: &[String]) -> Result<Captured> {
    Err(crate::error::MediaError::Unsupported)
}
