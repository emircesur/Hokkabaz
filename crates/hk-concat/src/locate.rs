//! Finds the `concat` executable: config path, `HK_CONCAT`, `PATH`, then the install locations
//! from Concat's docs/MCP.md.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::ConcatError;

pub const ENV_VAR: &str = "HK_CONCAT";

/// Environment lookups, injectable so tests do not touch the process environment.
pub trait Env {
    fn var(&self, key: &str) -> Option<OsString>;
}

pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key).filter(|v| !v.is_empty())
    }
}

/// Locates the binary using the real environment.
pub fn locate_binary(configured: Option<&Path>) -> Result<PathBuf, ConcatError> {
    locate_with(&ProcessEnv, configured)
}

pub fn locate_with(env: &dyn Env, configured: Option<&Path>) -> Result<PathBuf, ConcatError> {
    // An explicit choice that is wrong is an error, not a fall-through: silently driving a
    // different editor build than the one configured would be worse than failing.
    if let Some(p) = configured {
        return explicit(p, "`[concat] path`");
    }
    if let Some(p) = env.var(ENV_VAR) {
        return explicit(Path::new(&p), ENV_VAR);
    }
    let mut searched = Vec::new();
    for candidate in candidates(env) {
        if is_executable(&candidate) {
            return Ok(candidate);
        }
        searched.push(candidate.display().to_string());
    }
    Err(ConcatError::NotInstalled { searched })
}

fn explicit(p: &Path, what: &str) -> Result<PathBuf, ConcatError> {
    if is_executable(p) {
        Ok(p.to_owned())
    } else {
        Err(ConcatError::Spawn(format!(
            "{what} points at {}, which is not an executable file",
            p.display()
        )))
    }
}

/// `PATH` entries first, then well-known install dirs. Order is the search order.
pub fn candidates(env: &dyn Env) -> Vec<PathBuf> {
    let exe = if cfg!(windows) {
        "concat.exe"
    } else {
        "concat"
    };
    let mut out = Vec::new();
    if let Some(path) = env.var("PATH") {
        out.extend(std::env::split_paths(&path).map(|d| d.join(exe)));
    }
    let home = env
        .var("HOME")
        .or_else(|| env.var("USERPROFILE"))
        .map(PathBuf::from);

    // macOS app bundle.
    out.push(PathBuf::from(
        "/Applications/Concat.app/Contents/MacOS/concat",
    ));
    if let Some(h) = &home {
        out.push(h.join("Applications/Concat.app/Contents/MacOS/concat"));
    }
    // Windows: per-user installer, then per-machine / .msi.
    if let Some(l) = env.var("LOCALAPPDATA") {
        out.push(PathBuf::from(l).join("Programs/Concat/concat.exe"));
    }
    for pf in ["ProgramFiles", "ProgramW6432"] {
        if let Some(p) = env.var(pf) {
            out.push(PathBuf::from(p).join("Concat/concat.exe"));
        }
    }
    out.push(PathBuf::from("C:\\Program Files\\Concat\\concat.exe"));
    // Linux packages (.deb/.rpm/Arch) and local installs.
    for d in ["/usr/bin", "/usr/local/bin", "/opt/concat", "/opt/Concat"] {
        out.push(PathBuf::from(d).join(exe));
    }
    if let Some(h) = &home {
        for d in [".local/bin", "bin", ".cargo/bin"] {
            out.push(h.join(d).join(exe));
        }
        // AppImage: the file itself is the command.
        for d in ["Applications", ".local/bin", "Downloads", "bin"] {
            out.extend(appimages(&h.join(d)));
        }
    }
    out
}

fn appimages(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                let n = n.to_ascii_lowercase();
                n.starts_with("concat") && n.ends_with(".appimage")
            })
        })
        .collect();
    v.sort();
    v
}

fn is_executable(p: &Path) -> bool {
    let Ok(md) = std::fs::metadata(p) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct MapEnv(HashMap<&'static str, OsString>);
    impl Env for MapEnv {
        fn var(&self, key: &str) -> Option<OsString> {
            self.0.get(key).cloned()
        }
    }

    #[cfg(unix)]
    fn exe(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn precedence_config_then_env_then_path_then_known_dirs() {
        let t = tempfile::tempdir().unwrap();
        let (cfg, envp, pathd, home) = (
            t.path().join("cfg/concat"),
            t.path().join("env/concat"),
            t.path().join("bin"),
            t.path().join("home"),
        );
        let on_path = pathd.join("concat");
        let local = home.join(".local/bin/concat");
        for p in [&cfg, &envp, &on_path, &local] {
            exe(p);
        }
        let env = MapEnv(HashMap::from([
            ("HK_CONCAT", envp.clone().into_os_string()),
            ("PATH", pathd.clone().into_os_string()),
            ("HOME", home.clone().into_os_string()),
        ]));
        assert_eq!(locate_with(&env, Some(&cfg)).unwrap(), cfg);
        assert_eq!(locate_with(&env, None).unwrap(), envp);
        let env = MapEnv(HashMap::from([
            ("PATH", pathd.into_os_string()),
            ("HOME", home.clone().into_os_string()),
        ]));
        assert_eq!(locate_with(&env, None).unwrap(), on_path);
        std::fs::remove_file(&on_path).unwrap();
        // No PATH hit => the ~/.local/bin install is found.
        let env = MapEnv(HashMap::from([("HOME", home.into_os_string())]));
        assert_eq!(locate_with(&env, None).unwrap(), local);
    }

    #[cfg(unix)]
    #[test]
    fn appimage_in_applications_dir_is_found() {
        let t = tempfile::tempdir().unwrap();
        let img = t.path().join("Applications/Concat-0.2.8-x86_64.AppImage");
        exe(&img);
        let env = MapEnv(HashMap::from([("HOME", t.path().as_os_str().to_owned())]));
        assert_eq!(locate_with(&env, None).unwrap(), img);
    }

    #[test]
    fn wrong_explicit_path_is_an_error_not_a_fallthrough() {
        let env = MapEnv(HashMap::new());
        let e = locate_with(&env, Some(Path::new("/definitely/not/concat"))).unwrap_err();
        assert!(matches!(e, ConcatError::Spawn(m) if m.contains("[concat] path")));
        let env = MapEnv(HashMap::from([(
            "HK_CONCAT",
            OsString::from("/nope/concat"),
        )]));
        assert!(
            matches!(locate_with(&env, None), Err(ConcatError::Spawn(m)) if m.contains("HK_CONCAT"))
        );
    }

    #[test]
    fn not_installed_lists_what_was_searched() {
        let env = MapEnv(HashMap::from([(
            "HOME",
            OsString::from("/nonexistent-home"),
        )]));
        // Hosts that really have /usr/bin/concat would pass; the test only asserts the shape.
        if let Err(ConcatError::NotInstalled { searched }) = locate_with(&env, None) {
            assert!(searched.iter().any(|s| s.contains("/usr/bin")));
            let msg = ConcatError::NotInstalled { searched }.to_string();
            assert!(msg.contains("HK_CONCAT") && msg.contains("docs/CONCAT.md"));
        }
    }
}
