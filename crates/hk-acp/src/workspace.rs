//! Workspace confinement for `fs/*` client methods: agent-supplied paths must resolve inside
//! the session root, symlinks included.

use std::path::{Component, Path, PathBuf};

use crate::rpc::{RpcError, code};

/// Largest file `fs/read_text_file` returns.
pub const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new(root: &Path) -> std::io::Result<Self> {
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path inside the root, or an error. Does not require the file to exist.
    pub fn resolve(&self, raw: &str) -> Result<PathBuf, RpcError> {
        let p = Path::new(raw);
        if !p.is_absolute() {
            return Err(RpcError::invalid_params(format!("path must be absolute: {raw}")));
        }
        let norm = lexical_normalize(p);
        // Canonicalize the deepest existing ancestor (follows symlinks), then re-append the
        // not-yet-existing tail, which cannot contain links.
        let mut existing = norm.as_path();
        let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
        let base = loop {
            match existing.canonicalize() {
                Ok(c) => break c,
                Err(_) => {
                    let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
                        return Err(RpcError::invalid_params(format!("unresolvable path: {raw}")));
                    };
                    tail.push(name);
                    existing = parent;
                }
            }
        };
        let full = tail.iter().rev().fold(base, |acc, n| acc.join(n));
        if full.starts_with(&self.root) {
            Ok(full)
        } else {
            Err(RpcError::invalid_params(format!(
                "path outside workspace {}: {raw}",
                self.root.display()
            )))
        }
    }

    pub fn read_text(&self, raw: &str, line: Option<u32>, limit: Option<u32>) -> Result<String, RpcError> {
        let path = self.resolve(raw)?;
        let meta = std::fs::metadata(&path).map_err(|e| io_err(&e, raw))?;
        if meta.len() > MAX_READ_BYTES {
            return Err(RpcError::invalid_params(format!(
                "file too large ({} bytes, max {MAX_READ_BYTES}): {raw}",
                meta.len()
            )));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| io_err(&e, raw))?;
        if line.is_none() && limit.is_none() {
            return Ok(text);
        }
        let skip = line.map_or(0, |l| l.saturating_sub(1) as usize);
        let take = limit.map_or(usize::MAX, |l| l as usize);
        Ok(text.split_inclusive('\n').skip(skip).take(take).collect())
    }

    pub fn write_text(&self, raw: &str, content: &str) -> Result<(), RpcError> {
        let path = self.resolve(raw)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io_err(&e, raw))?;
        }
        std::fs::write(&path, content).map_err(|e| io_err(&e, raw))
    }
}

fn io_err(e: &std::io::Error, raw: &str) -> RpcError {
    if e.kind() == std::io::ErrorKind::NotFound {
        RpcError::new(code::RESOURCE_NOT_FOUND, format!("not found: {raw}"))
    } else {
        RpcError::internal(format!("{raw}: {e}"))
    }
}

fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> (tempfile::TempDir, Workspace) {
        let d = tempfile::tempdir().unwrap();
        let w = Workspace::new(d.path()).unwrap();
        (d, w)
    }

    #[test]
    fn inside_ok_outside_rejected() {
        let (d, w) = ws();
        let inside = d.path().join("a/b.txt");
        w.write_text(inside.to_str().unwrap(), "x\ny\nz\n").unwrap();
        assert_eq!(w.read_text(inside.to_str().unwrap(), None, None).unwrap(), "x\ny\nz\n");
        assert_eq!(w.read_text(inside.to_str().unwrap(), Some(2), Some(1)).unwrap(), "y\n");
        assert!(w.resolve("/etc/passwd").is_err());
        assert!(w.resolve("relative.txt").is_err());
        let dotdot = format!("{}/../outside.txt", d.path().display());
        assert!(w.resolve(&dotdot).is_err());
        assert!(w.write_text(&dotdot, "no").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_rejected() {
        let (d, w) = ws();
        let out = tempfile::tempdir().unwrap();
        std::fs::write(out.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(out.path(), d.path().join("link")).unwrap();
        let via = format!("{}/link/secret", d.path().display());
        assert!(w.read_text(&via, None, None).is_err());
        assert!(w.write_text(&format!("{}/link/new", d.path().display()), "x").is_err());
    }

    #[test]
    fn missing_file_maps_to_not_found() {
        let (d, w) = ws();
        let e = w.read_text(d.path().join("nope").to_str().unwrap(), None, None).unwrap_err();
        assert_eq!(e.code, code::RESOURCE_NOT_FOUND);
    }
}
