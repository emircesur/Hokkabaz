use std::time::Duration;

/// Everything the bridge can fail with. `OutsideRoots` and `NotInstalled` carry the fix in their
/// text because the model reads these messages verbatim.
#[derive(Debug, thiserror::Error)]
pub enum ConcatError {
    /// Target without a process bridge (android) or build without the needed feature.
    #[error("Concat is unsupported here: {0}")]
    Unsupported(&'static str),
    #[error(
        "Concat binary not found (looked at: {}). Install Concat (docs/CONCAT.md), then set \
         `[concat] path`, the HK_CONCAT env var, or put `concat` on PATH",
        .searched.join(", ")
    )]
    NotInstalled { searched: Vec<String> },
    #[error("could not start Concat: {0}")]
    Spawn(String),
    #[error("Concat protocol error: {0}")]
    Protocol(String),
    #[error("Concat closed the connection{}", tail(.stderr))]
    Closed { stderr: String },
    #[error("Concat did not answer within {0:?}")]
    Timeout(Duration),
    /// A write target outside the folders Concat may write to (home, or `CONCAT_MCP_ROOTS`).
    #[error(
        "{path} is outside the folders Concat may write to ({}). Concat only writes projects, \
         exports and frames under your home folder or CONCAT_MCP_ROOTS: use a path there, or add \
         the folder to `[concat] roots`",
        .roots.join(", ")
    )]
    OutsideRoots { path: String, roots: Vec<String> },
    /// Concat refused a call; `message` is the sentence the Concat window would show.
    #[error("Concat refused `{tool}`: {message}{}", code_suffix(.code))]
    Tool {
        tool: String,
        code: Option<String>,
        message: String,
    },
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("invalid Studio plan: {0}")]
    InvalidPlan(String),
    #[error("unexpected Concat reply to `{tool}`: {reason}")]
    Decode { tool: String, reason: String },
}

fn tail(stderr: &str) -> String {
    if stderr.trim().is_empty() {
        String::new()
    } else {
        format!(" (stderr: {})", stderr.trim())
    }
}

fn code_suffix(code: &Option<String>) -> String {
    code.as_ref().map(|c| format!(" (code: {c})")).unwrap_or_default()
}

impl ConcatError {
    pub(crate) fn decode(tool: &str, reason: impl std::fmt::Display) -> Self {
        Self::Decode { tool: tool.to_owned(), reason: reason.to_string() }
    }

    /// The process is gone or wedged; a caller holding a cached client should reconnect.
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, Self::Closed { .. } | Self::Timeout(_) | Self::Protocol(_))
    }

    /// Maps the text of a Concat tool error (`"<sentence> (code: <code>)"`) to a typed error.
    /// Roots violations come from `Api::writable` in concat-api:
    /// `"<path> is outside where this API writes (<root>, <root>)"`.
    pub(crate) fn from_tool_text(tool: &str, text: &str) -> Self {
        const MARK: &str = " is outside where this API writes (";
        if let Some(at) = text.find(MARK) {
            let path = text[..at].trim().to_owned();
            let rest = &text[at + MARK.len()..];
            let roots = rest
                .split_once(')')
                .map(|(r, _)| r)
                .unwrap_or(rest)
                .split(", ")
                .map(|r| r.trim().to_owned())
                .filter(|r| !r.is_empty())
                .collect();
            return Self::OutsideRoots { path, roots };
        }
        let (message, code) = match text.trim_end().rsplit_once(" (code: ") {
            Some((m, c)) if c.ends_with(')') => (m.to_owned(), Some(c.trim_end_matches(')').to_owned())),
            _ => (text.trim().to_owned(), None),
        };
        Self::Tool { tool: tool.to_owned(), code, message }
    }
}
