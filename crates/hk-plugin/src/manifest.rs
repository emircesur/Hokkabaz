//! `plugin.toml` and the capability model.
//!
//! Capabilities are deny-by-default: a plugin that declares nothing can compute and log, nothing
//! else. The user sees the list before installing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{PluginError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsGrant {
    /// Directory on the host (absolute).
    pub path: PathBuf,
    #[serde(default)]
    pub write: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Capabilities {
    /// Host patterns the plugin may call over HTTP(S): `example.com` or `*.example.com`.
    pub http: Vec<String>,
    /// Per-plugin key/value store.
    pub kv: bool,
    /// Secret names the plugin may read.
    pub secrets: Vec<String>,
    /// May emit trigger events.
    pub emit: bool,
    /// Host directories mapped into the plugin (WASI), at the same path.
    pub fs: Vec<FsGrant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Reverse-DNS style id, `[a-z0-9._-]`, e.g. `dev.hokkabaz.hello`.
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Component file next to the manifest.
    pub wasm: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub capabilities: Capabilities,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self> {
        let m: Manifest = toml::from_str(text).map_err(|e| PluginError::Manifest(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    pub fn load(dir: &Path) -> Result<Self> {
        Self::parse(&std::fs::read_to_string(dir.join("plugin.toml"))?)
    }

    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(PluginError::Manifest(m.to_string()));
        if self.id.is_empty()
            || self.id.len() > 100
            || !self.id.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            })
            || self.id.starts_with(['.', '-'])
            || self.id.contains("..")
        {
            return bad("`id` must be 1-100 chars of [a-z0-9._-] and not start with '.' or '-'");
        }
        if self.name.trim().is_empty() {
            return bad("`name` is required");
        }
        if self.version.split('.').count() != 3
            || !self.version.split('.').all(|p| p.parse::<u32>().is_ok())
        {
            return bad("`version` must be MAJOR.MINOR.PATCH");
        }
        let w = Path::new(&self.wasm);
        if self.wasm.is_empty()
            || w.is_absolute()
            || w.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            || !self.wasm.ends_with(".wasm")
        {
            return bad("`wasm` must be a relative .wasm file next to plugin.toml");
        }
        for p in &self.capabilities.http {
            if !valid_host_pattern(p) {
                return Err(PluginError::Manifest(format!(
                    "bad http host pattern `{p}`"
                )));
            }
        }
        for g in &self.capabilities.fs {
            if !g.path.is_absolute() {
                return Err(PluginError::Manifest(format!(
                    "fs grant `{}` must be an absolute path",
                    g.path.display()
                )));
            }
        }
        Ok(())
    }

    /// Short human summary of what the plugin may do (shown before install).
    pub fn capability_summary(&self) -> Vec<String> {
        let c = &self.capabilities;
        let mut out = Vec::new();
        if !c.http.is_empty() {
            out.push(format!("Network access to: {}", c.http.join(", ")));
        }
        if c.kv {
            out.push("Store its own data".into());
        }
        if !c.secrets.is_empty() {
            out.push(format!("Read secrets: {}", c.secrets.join(", ")));
        }
        if c.emit {
            out.push("Start flows (trigger events)".into());
        }
        for g in &c.fs {
            out.push(format!(
                "{} files in {}",
                if g.write { "Read and write" } else { "Read" },
                g.path.display()
            ));
        }
        if out.is_empty() {
            out.push("No special permissions".into());
        }
        out
    }
}

fn valid_host_pattern(p: &str) -> bool {
    let host = p.strip_prefix("*.").unwrap_or(p);
    !host.is_empty()
        && host.len() <= 253
        && !host.contains(['/', ':', ' ', '*'])
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
}

impl Capabilities {
    /// May the plugin call `url`? Only http(s), and the host must match a granted pattern.
    pub fn check_url(&self, url: &str) -> Result<Url> {
        let u = Url::parse(url).map_err(|e| PluginError::Denied(format!("bad URL: {e}")))?;
        if !matches!(u.scheme(), "http" | "https") {
            return Err(PluginError::Denied(format!(
                "scheme `{}` is not allowed",
                u.scheme()
            )));
        }
        let host = u
            .host_str()
            .ok_or_else(|| PluginError::Denied("URL has no host".into()))?
            .to_ascii_lowercase();
        if self.http.iter().any(|p| host_matches(p, &host)) {
            Ok(u)
        } else {
            Err(PluginError::Denied(format!(
                "http access to `{host}` was not granted"
            )))
        }
    }

    pub fn check_secret(&self, name: &str) -> Result<()> {
        if self.secrets.iter().any(|s| s == name) {
            Ok(())
        } else {
            Err(PluginError::Denied(format!(
                "secret `{name}` was not granted"
            )))
        }
    }

    pub fn check_kv(&self) -> Result<()> {
        if self.kv {
            Ok(())
        } else {
            Err(PluginError::Denied(
                "key/value storage was not granted".into(),
            ))
        }
    }

    pub fn check_emit(&self) -> Result<()> {
        if self.emit {
            Ok(())
        } else {
            Err(PluginError::Denied(
                "emitting events was not granted".into(),
            ))
        }
    }
}

fn host_matches(pattern: &str, host: &str) -> bool {
    let p = pattern.to_ascii_lowercase();
    match p.strip_prefix("*.") {
        Some(suffix) => {
            host.len() > suffix.len()
                && host.ends_with(suffix)
                && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
        }
        None => host == p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"
id = "dev.example.demo"
name = "Demo"
version = "1.2.3"
wasm = "demo.wasm"
[capabilities]
http = ["api.example.com", "*.cdn.example.com"]
kv = true
secrets = ["demo_key"]
"#;

    #[test]
    fn parses_and_summarizes() {
        let m = Manifest::parse(OK).unwrap();
        assert_eq!(m.id, "dev.example.demo");
        let s = m.capability_summary();
        assert!(s[0].contains("api.example.com") && s.iter().any(|l| l.contains("demo_key")));
        assert_eq!(
            Manifest::parse("id='a.b'\nname='x'\nversion='0.0.1'\nwasm='x.wasm'")
                .unwrap()
                .capability_summary(),
            ["No special permissions"]
        );
    }

    #[test]
    fn rejects_bad_manifests() {
        for (frag, why) in [
            ("id = \"Bad Id\"", "uppercase/space id"),
            ("version = \"1.2\"", "short version"),
            ("wasm = \"../evil.wasm\"", "path traversal"),
            ("wasm = \"/abs.wasm\"", "absolute wasm"),
            ("wasm = \"x.txt\"", "not wasm"),
        ] {
            let key = frag.split(' ').next().unwrap();
            let text: String = OK
                .lines()
                .map(|l| {
                    if l.starts_with(key) {
                        frag.to_string()
                    } else {
                        l.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(Manifest::parse(&text).is_err(), "{why}");
        }
        assert!(
            Manifest::parse(&OK.replace("kv = true", "kv = true\nbogus = 1")).is_err(),
            "unknown capability key"
        );
        assert!(
            Manifest::parse(&OK.replace("api.example.com", "https://x/y")).is_err(),
            "pattern with scheme"
        );
        assert!(
            Manifest::parse(&OK.replace("*.cdn.example.com", "*")).is_err(),
            "bare wildcard"
        );
    }

    #[test]
    fn url_checks() {
        let c = Manifest::parse(OK).unwrap().capabilities;
        assert!(c.check_url("https://api.example.com/v1/x").is_ok());
        assert!(c.check_url("http://img.cdn.example.com/a.png").is_ok());
        assert!(
            c.check_url("https://cdn.example.com/").is_err(),
            "wildcard needs a subdomain"
        );
        assert!(c.check_url("https://evilapi.example.com/").is_err());
        assert!(c.check_url("https://api.example.com.evil.net/").is_err());
        assert!(c.check_url("file:///etc/passwd").is_err());
        assert!(c.check_url("http://127.0.0.1/").is_err());
        assert!(c.check_url("not a url").is_err());
        assert!(c.check_secret("demo_key").is_ok() && c.check_secret("other").is_err());
        assert!(c.check_kv().is_ok() && c.check_emit().is_err());
        assert!(Capabilities::default().check_kv().is_err());
    }
}
