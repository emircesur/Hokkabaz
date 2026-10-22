use std::path::PathBuf;

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
    fn env_override_and_default() {
        let t = Tools::resolve(|k| (k == ENV_FFMPEG).then(|| "/opt/ff/ffmpeg".to_string()));
        assert_eq!(t.ffmpeg, PathBuf::from("/opt/ff/ffmpeg"));
        assert_eq!(t.ffprobe, PathBuf::from("ffprobe"));
        let t = Tools::resolve(|_| Some(String::new()));
        assert_eq!(t.ffmpeg, PathBuf::from("ffmpeg"));
    }
}
