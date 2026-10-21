use std::path::{Path, PathBuf};

/// Limits for model-visible tool output.
#[derive(Debug, Clone)]
pub struct OutputLimits {
    /// Max bytes of `content` kept inline. Larger output spills to `spill_dir`.
    pub max_bytes: usize,
    pub spill_dir: PathBuf,
}

impl OutputLimits {
    pub fn new(spill_dir: impl Into<PathBuf>) -> Self {
        Self {
            max_bytes: 50 * 1024,
            spill_dir: spill_dir.into(),
        }
    }
}

#[derive(Debug)]
pub struct Spilled {
    pub content: String,
    pub path: Option<PathBuf>,
    pub original_bytes: usize,
}

/// Cut `text` to `limits.max_bytes` (head + tail, UTF-8 safe) and write the full text to
/// `<spill_dir>/<call_id>.txt`. Returns `None` when it already fits. A failed spill write still
/// truncates (path is `None`): the model must never get an unbounded result.
pub async fn truncate_and_spill(
    call_id: &str,
    text: &str,
    limits: &OutputLimits,
) -> Option<Spilled> {
    if text.len() <= limits.max_bytes {
        return None;
    }
    let path = limits.spill_dir.join(format!("{}.txt", sanitize(call_id)));
    let path = match write(&path, text).await {
        Ok(()) => Some(path),
        Err(e) => {
            tracing::warn!(error = %e, "tool output spill failed; truncating only");
            None
        }
    };
    let note = match &path {
        Some(p) => format!(
            "\n[... {} bytes omitted; full output: {} ...]\n",
            text.len() - limits.max_bytes,
            p.display()
        ),
        None => format!(
            "\n[... {} bytes omitted ...]\n",
            text.len() - limits.max_bytes
        ),
    };
    let head_len = floor_boundary(text, limits.max_bytes / 2);
    let tail_start = ceil_boundary(text, text.len() - (limits.max_bytes - limits.max_bytes / 2));
    let content = format!("{}{}{}", &text[..head_len], note, &text[tail_start..]);
    Some(Spilled {
        content,
        path,
        original_bytes: text.len(),
    })
}

async fn write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::write(path, text).await
}

/// Call ids come from the model; keep them from escaping the spill dir.
fn sanitize(id: &str) -> String {
    let s: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "call".into() } else { s }
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}
