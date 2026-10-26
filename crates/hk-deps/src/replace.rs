//! Replace a file that may be in use (Windows): os error 5/32 when the target is a running
//! executable, a memory-mapped model, or being scanned by antivirus right after download.

use std::path::Path;
use std::time::Duration;

fn busy(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
        || matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

/// Move `tmp` onto `dest`. A locked `dest` is renamed aside first (Windows allows renaming a
/// running exe); transient locks are retried with backoff.
pub fn replace_file(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    let mut last = None;
    for attempt in 0..10u64 {
        match std::fs::rename(tmp, dest) {
            Ok(()) => return Ok(()),
            Err(e) if busy(&e) => {
                if dest.exists() {
                    let stamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis());
                    let name = dest
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let aside = dest.with_file_name(format!("{name}.old-{stamp}"));
                    if std::fs::rename(dest, &aside).is_ok() {
                        // Deleting fails while the old one still runs; `cleanup_aside` retries later.
                        let _ = std::fs::remove_file(&aside);
                    }
                }
                last = Some(e);
                std::thread::sleep(Duration::from_millis(150 * (attempt + 1)));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("cannot replace file")))
}

/// Remove `*.old-*` leftovers from earlier replacements in `dir` (best effort).
pub fn cleanup_aside(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().contains(".old-") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_existing() {
        let d = std::env::temp_dir().join(format!("hk_replace_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let (tmp, dest) = (d.join("a.part"), d.join("a.exe"));
        std::fs::write(&dest, "old").unwrap();
        std::fs::write(&tmp, "new").unwrap();
        replace_file(&tmp, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
        cleanup_aside(&d);
        let _ = std::fs::remove_dir_all(&d);
    }
}
