//! Sandbox policy model: write roots, network on/off, env allowlist, optional command allowlist.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::SandboxError;

/// Which enforcement mechanism to use. `Auto` resolves via [`BackendKind::resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    #[default]
    Auto,
    Landlock,
    Seatbelt,
    /// No kernel enforcement: env filtering + command/path allowlists only.
    None,
}

impl BackendKind {
    /// Pick the strongest backend this build+host supports. Never returns `Auto`.
    pub fn resolve(self) -> BackendKind {
        match self {
            BackendKind::Auto => {
                if cfg!(target_os = "macos") {
                    BackendKind::Seatbelt
                } else if crate::landlock_available() {
                    BackendKind::Landlock
                } else {
                    BackendKind::None
                }
            }
            other => other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub backend: BackendKind,
    /// Subtrees (or single files) that may be written. Everything else is read-only.
    pub write_roots: Vec<PathBuf>,
    /// Outbound/inbound network. Landlock only gates TCP bind/connect (ABI v4+).
    #[serde(default)]
    pub network: bool,
    /// Env var names passed to the child; everything else is dropped.
    #[serde(default = "default_env_allowlist")]
    pub env_allowlist: Vec<String>,
    /// If set, `argv[0]` basename must be listed. Enforced by every backend.
    #[serde(default)]
    pub command_allowlist: Option<Vec<String>>,
    /// Fail closed when the kernel cannot enforce the policy (vs. run unsandboxed).
    #[serde(default = "yes")]
    pub strict: bool,
}

fn yes() -> bool {
    true
}

pub fn default_env_allowlist() -> Vec<String> {
    [
        "PATH", "HOME", "USER", "LANG", "LC_ALL", "TERM", "TZ", "TMPDIR", "SHELL",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

impl Policy {
    /// Writable: `root`, temp dirs, and the usual std device nodes. Network off.
    pub fn for_workspace(root: impl Into<PathBuf>) -> Self {
        let mut write_roots = vec![root.into(), std::env::temp_dir()];
        if cfg!(unix) {
            write_roots.push(PathBuf::from("/tmp"));
            for dev in ["null", "zero", "full", "tty", "random", "urandom"] {
                let p = Path::new("/dev").join(dev);
                if p.exists() {
                    write_roots.push(p);
                }
            }
        }
        Self {
            backend: BackendKind::Auto,
            write_roots,
            network: false,
            env_allowlist: default_env_allowlist(),
            command_allowlist: None,
            strict: true,
        }
    }

    /// Check a path a file tool wants to write. Resolves `..` and symlinks (on the deepest
    /// existing ancestor) so neither can escape the roots.
    pub fn check_write(&self, path: &Path) -> Result<(), SandboxError> {
        if !path.is_absolute() {
            return Err(SandboxError::NotAbsolute(path.to_path_buf()));
        }
        let target = resolve(path);
        let allowed = self
            .write_roots
            .iter()
            .filter(|r| r.is_absolute())
            .any(|r| target.starts_with(resolve(r)));
        if allowed {
            Ok(())
        } else {
            Err(SandboxError::WriteDenied(path.to_path_buf()))
        }
    }

    pub(crate) fn check_command(&self, argv0: &str) -> Result<(), SandboxError> {
        let Some(list) = &self.command_allowlist else {
            return Ok(());
        };
        let base = Path::new(argv0)
            .file_name()
            .map_or(argv0.to_string(), |s| s.to_string_lossy().into_owned());
        if list.iter().any(|a| a == &base) {
            Ok(())
        } else {
            Err(SandboxError::CommandNotAllowed(base))
        }
    }

    /// Write roots that exist, canonicalized (kernel rules need real paths; macOS /tmp is a symlink).
    pub(crate) fn existing_roots(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for r in self.write_roots.iter().filter(|r| r.is_absolute()) {
            if let Ok(c) = r.canonicalize() {
                if !out.contains(&c) {
                    out.push(c);
                }
            }
        }
        out
    }
}

/// Lexically normalize, then canonicalize the deepest existing ancestor and re-append the rest.
fn resolve(path: &Path) -> PathBuf {
    let mut norm = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                norm.pop();
            }
            Component::CurDir => {}
            other => norm.push(other.as_os_str()),
        }
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = norm.as_path();
    loop {
        if let Ok(real) = cur.canonicalize() {
            let mut out = real;
            out.extend(tail.iter().rev());
            return out;
        }
        match (cur.parent(), cur.file_name()) {
            (Some(p), Some(name)) => {
                tail.push(name.to_os_string());
                cur = p;
            }
            _ => return norm,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(root: &Path) -> Policy {
        Policy {
            backend: BackendKind::None,
            write_roots: vec![root.to_path_buf()],
            network: false,
            env_allowlist: default_env_allowlist(),
            command_allowlist: None,
            strict: true,
        }
    }

    #[test]
    fn inside_root_allowed_even_if_missing() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path());
        p.check_write(&d.path().join("a/b/new.txt")).unwrap();
    }

    #[test]
    fn outside_root_denied() {
        let d = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let p = policy(d.path());
        assert!(matches!(
            p.check_write(&other.path().join("x")),
            Err(SandboxError::WriteDenied(_))
        ));
    }

    #[test]
    fn dotdot_escape_denied() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path());
        assert!(p.check_write(&d.path().join("../escape")).is_err());
    }

    #[test]
    fn sibling_prefix_not_confused() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("proj");
        std::fs::create_dir(&root).unwrap();
        let p = policy(&root);
        assert!(p.check_write(&d.path().join("proj-evil/x")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_denied() {
        let d = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let root = d.path().join("proj");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(out.path(), root.join("link")).unwrap();
        let p = policy(&root);
        assert!(p.check_write(&root.join("link/x")).is_err());
    }

    #[test]
    fn relative_path_rejected() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(
            policy(d.path()).check_write(Path::new("rel")),
            Err(SandboxError::NotAbsolute(_))
        ));
    }

    #[test]
    fn command_allowlist_uses_basename() {
        let d = tempfile::tempdir().unwrap();
        let mut p = policy(d.path());
        p.command_allowlist = Some(vec!["git".into()]);
        p.check_command("/usr/bin/git").unwrap();
        assert!(p.check_command("rm").is_err());
    }

    #[test]
    fn serde_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = policy(d.path());
        let back: Policy = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(p, back);
    }
}
