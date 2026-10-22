use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

/// Env var overriding the ffmpeg binary path.
pub const ENV_FFMPEG: &str = "HK_FFMPEG";
/// Env var overriding the ffprobe binary path.
pub const ENV_FFPROBE: &str = "HK_FFPROBE";

/// Locations of the ffmpeg/ffprobe executables. Bare names resolve via `PATH` at spawn time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

static MANAGED_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Folder the app downloads ffmpeg/ffprobe into. Bare program names resolve there before `PATH`
/// (see [`resolve_program`]), so a tool installed from the app is picked up by running sessions.
pub fn set_managed_dir(dir: Option<PathBuf>) {
    *MANAGED_DIR.write().unwrap_or_else(PoisonError::into_inner) = dir;
}

pub fn managed_dir() -> Option<PathBuf> {
    MANAGED_DIR
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// The program to spawn for `p`. An explicit path (from `HK_FFMPEG` or config) is used as is; the
/// bare names `ffmpeg` / `ffprobe` prefer the managed folder, then fall back to `PATH`.
pub fn resolve_program(p: &Path) -> PathBuf {
    let bare = p.components().count() == 1;
    if bare
        && let Some(name) = p.to_str()
        && matches!(name, "ffmpeg" | "ffprobe")
        && let Some(dir) = managed_dir()
    {
        let exe = dir.join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        });
        if exe.is_file() {
            return exe;
        }
    }
    p.to_owned()
}

impl Tools {
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
        }
    }

    /// `HK_FFMPEG` / `HK_FFPROBE` if set and non-empty, else `ffmpeg` / `ffprobe` from `PATH`.
    pub fn from_env() -> Self {
        Self::resolve(|k| std::env::var(k).ok())
    }

    pub(crate) fn resolve(get: impl Fn(&str) -> Option<String>) -> Self {
        let pick = |key: &str, default: &str| {
            get(key)
                .filter(|v| !v.is_empty())
                .map_or_else(|| PathBuf::from(default), PathBuf::from)
        };
        Self {
            ffmpeg: pick(ENV_FFMPEG, "ffmpeg"),
            ffprobe: pick(ENV_FFPROBE, "ffprobe"),
        }
    }
}

impl Default for Tools {
    fn default() -> Self {
        Self::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_dir_beats_path_but_not_explicit() {
        let d = std::env::temp_dir().join(format!("hk-managed-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let exe = d.join(if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        });
        std::fs::write(&exe, b"x").unwrap();
        set_managed_dir(Some(d.clone()));
        assert_eq!(resolve_program(Path::new("ffmpeg")), exe);
        // ffprobe is not there: stays a PATH lookup
        assert_eq!(
            resolve_program(Path::new("ffprobe")),
            PathBuf::from("ffprobe")
        );
        // an explicit path is never rewritten
        assert_eq!(
            resolve_program(Path::new("/opt/ff/ffmpeg")),
            PathBuf::from("/opt/ff/ffmpeg")
        );
        set_managed_dir(None);
        assert_eq!(
            resolve_program(Path::new("ffmpeg")),
            PathBuf::from("ffmpeg")
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn env_override_and_default() {
        let t = Tools::resolve(|k| (k == ENV_FFMPEG).then(|| "/opt/ff/ffmpeg".to_string()));
        assert_eq!(t.ffmpeg, PathBuf::from("/opt/ff/ffmpeg"));
        assert_eq!(t.ffprobe, PathBuf::from("ffprobe"));
        let t = Tools::resolve(|_| Some(String::new()));
        assert_eq!(t.ffmpeg, PathBuf::from("ffmpeg"));
    }
}
