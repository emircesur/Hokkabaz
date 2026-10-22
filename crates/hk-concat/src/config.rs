use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ConcatError;

/// Which Concat server `concat --mcp` talks to (Concat docs/MCP.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConcatMode {
    /// The open Concat window's automation server when reachable, else headless. Safest while
    /// a person is editing: the agent cannot open the project on screen.
    #[default]
    Auto,
    /// Own engine inside the `concat --mcp` process; nothing has to be open.
    Headless,
    /// Only the window's server (Settings > Automation > Scripts and apps); fail otherwise.
    Window,
}

impl ConcatMode {
    pub fn as_arg(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Headless => "headless",
            Self::Window => "window",
        }
    }
}

/// Which implementation backs [`crate::ConcatClient`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// External `concat --mcp` child process.
    #[default]
    Process,
    /// In-process engine (cargo feature `engine`).
    Engine,
}

fn default_timeout() -> u64 {
    300
}

/// The `[concat]` config section.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConcatConfig {
    /// Explicit path to the `concat` binary (or an AppImage). Beats HK_CONCAT and PATH.
    pub path: Option<PathBuf>,
    pub backend: BackendKind,
    pub mode: ConcatMode,
    /// Extra folders Concat may write to (headless mode); becomes `CONCAT_MCP_ROOTS`.
    pub roots: Vec<PathBuf>,
    /// Remote Concat server (`CONCAT_API_ADDRESS`, e.g. `127.0.0.1:7420`). Needs `api_token`.
    pub api_address: Option<String>,
    /// `CONCAT_API_TOKEN`; a resolved secret, never logged.
    pub api_token: Option<String>,
    /// Per-call timeout. Edits are fast; exports are polled, not awaited in one call.
    pub timeout_secs: u64,
}

impl Default for ConcatConfig {
    fn default() -> Self {
        Self {
            path: None,
            backend: BackendKind::default(),
            mode: ConcatMode::default(),
            roots: Vec::new(),
            api_address: None,
            api_token: None,
            timeout_secs: default_timeout(),
        }
    }
}

// Manual Debug: the token must never reach logs.
impl fmt::Debug for ConcatConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConcatConfig")
            .field("path", &self.path)
            .field("backend", &self.backend)
            .field("mode", &self.mode)
            .field("roots", &self.roots)
            .field("api_address", &self.api_address)
            .field("api_token", &self.api_token.as_ref().map(|_| "<redacted>"))
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

impl ConcatConfig {
    pub(crate) fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout_secs.max(1))
    }

    /// Roots as Concat will enforce them in headless mode: `roots` if set, else `$HOME`.
    /// `None` when unknowable (window/remote server decides on its own).
    pub fn effective_roots(&self) -> Option<RootsPolicy> {
        if self.mode == ConcatMode::Window || self.api_address.is_some() {
            return None;
        }
        if !self.roots.is_empty() {
            // Concat's headless mode REPLACES the home default when CONCAT_MCP_ROOTS is set.
            return Some(RootsPolicy::new(self.roots.clone()));
        }
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
        Some(RootsPolicy::new(vec![PathBuf::from(home)]))
    }
}

/// Concat's write confinement, mirrored so errors can name the fix before the call is made.
/// Lexical check (no symlink resolution): Concat does the authoritative one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootsPolicy {
    roots: Vec<PathBuf>,
}

impl RootsPolicy {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn check(&self, path: impl AsRef<Path>) -> Result<(), ConcatError> {
        let path = path.as_ref();
        let ok = path.is_absolute()
            && !path.components().any(|c| matches!(c, Component::ParentDir))
            && self.roots.iter().any(|r| path.starts_with(r));
        if ok {
            Ok(())
        } else {
            Err(ConcatError::OutsideRoots {
                path: path.display().to_string(),
                roots: self.roots.iter().map(|r| r.display().to_string()).collect(),
            })
        }
    }
}
