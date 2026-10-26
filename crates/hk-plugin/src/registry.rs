//! The plugin registry: a static `index.json` (GitHub repo `hokkabaz-plugins`, served raw) whose
//! entries point at release artifacts (`.tar.gz` with `plugin.toml` + wasm) and carry their
//! SHA-256. Nothing is installed unless the downloaded bytes hash to the listed value.
//!
//! ```json
//! {"version":1,"plugins":[{"id":"dev.example.demo","name":"Demo","version":"1.0.0",
//!   "description":"…","author":"…","license":"…","homepage":"…",
//!   "capabilities":{"http":["api.example.com"]},
//!   "download_url":"https://github.com/…/releases/download/v1.0.0/demo.tar.gz",
//!   "sha256":"<hex>","size":123456}],
//!  "templates":[{"id":"dev.example.triage","name":"Triage","version":"1.0.0","kind":"flow",
//!   "download_url":"https://…/triage.tar.gz","sha256":"<hex>","size":2048}]}
//! ```
//!
//! `templates` (D19) is optional; template packages are described in [`crate::template`].

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{PluginError, Result};
use crate::manifest::Capabilities;
use crate::template::{TemplateKind, TemplatePackage};

pub const DEFAULT_INDEX_URL: &str =
    "https://raw.githubusercontent.com/emircesur/hokkabaz-plugins/main/index.json";
const MAX_ARTIFACT_BYTES: u64 = 140 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub capabilities: Capabilities,
    pub download_url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

/// A template listed in the same index as plugins. Mirrors the package's `template.toml` so the
/// gallery can filter without downloading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: TemplateKind,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_hokkabaz_version: Option<String>,
    pub download_url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
}

impl TemplateEntry {
    /// Index entry for a packed template (`.tar.gz`), as published at `download_url`.
    pub fn for_archive(bytes: &[u8], download_url: &str) -> Result<Self> {
        let m = TemplatePackage::from_archive(bytes)?.manifest;
        Ok(Self {
            id: m.id,
            name: m.name,
            version: m.version,
            kind: m.kind,
            description: m.description,
            tags: m.tags,
            author: m.author,
            min_hokkabaz_version: m.min_hokkabaz_version,
            download_url: download_url.to_string(),
            sha256: sha256_hex(bytes),
            size: bytes.len() as u64,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryIndex {
    pub version: u32,
    pub plugins: Vec<RegistryEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<TemplateEntry>,
}

fn check_artifact(what: &str, id: &str, sha256: &str, url: &str) -> Result<()> {
    if sha256.len() != 64 || !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(PluginError::Registry(format!(
            "{what} `{id}` has an invalid sha256"
        )));
    }
    if !url.starts_with("https://") {
        return Err(PluginError::Registry(format!(
            "{what} `{id}` must be downloaded over https"
        )));
    }
    Ok(())
}

impl RegistryIndex {
    pub fn parse(text: &str) -> Result<Self> {
        let idx: RegistryIndex = serde_json::from_str(text)
            .map_err(|e| PluginError::Registry(format!("bad index.json: {e}")))?;
        if idx.version != 1 {
            return Err(PluginError::Registry(format!(
                "unsupported registry version {}",
                idx.version
            )));
        }
        let mut seen = std::collections::HashSet::new();
        for e in &idx.plugins {
            if !seen.insert(e.id.clone()) {
                return Err(PluginError::Registry(format!(
                    "duplicate plugin id `{}`",
                    e.id
                )));
            }
            check_artifact("plugin", &e.id, &e.sha256, &e.download_url)?;
        }
        let mut seen = std::collections::HashSet::new();
        for e in &idx.templates {
            if !seen.insert(e.id.as_str()) {
                return Err(PluginError::Registry(format!(
                    "duplicate template id `{}`",
                    e.id
                )));
            }
            if parse_version(&e.version).is_none() {
                return Err(PluginError::Registry(format!(
                    "template `{}` has a bad version",
                    e.id
                )));
            }
            check_artifact("template", &e.id, &e.sha256, &e.download_url)?;
        }
        Ok(idx)
    }

    pub fn find(&self, id: &str) -> Option<&RegistryEntry> {
        self.plugins.iter().find(|e| e.id == id)
    }

    pub fn find_template(&self, id: &str) -> Option<&TemplateEntry> {
        self.templates.iter().find(|e| e.id == id)
    }

    /// Entries newer than what is installed: `(entry, installed_version)`.
    pub fn updates<'a>(
        &'a self,
        installed: &'a [(String, String)],
    ) -> Vec<(&'a RegistryEntry, &'a str)> {
        installed
            .iter()
            .filter_map(|(id, v)| {
                self.find(id)
                    .filter(|e| is_newer(&e.version, v))
                    .map(|e| (e, v.as_str()))
            })
            .collect()
    }
}

/// `MAJOR.MINOR.PATCH` -> numbers; `None` for anything else.
pub(crate) fn parse_version(v: &str) -> Option<[u32; 3]> {
    let mut it = v.split('.').map(|p| p.parse::<u32>().ok());
    let r = [it.next()??, it.next()??, it.next()??];
    it.next().is_none().then_some(r)
}

/// `candidate` > `installed` (MAJOR.MINOR.PATCH).
pub fn is_newer(candidate: &str, installed: &str) -> bool {
    matches!((parse_version(candidate), parse_version(installed)), (Some(a), Some(b)) if a > b)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Check downloaded bytes against the index entry.
pub fn verify(entry: &RegistryEntry, bytes: &[u8]) -> Result<()> {
    verify_digest(&entry.id, entry.size, &entry.sha256, bytes)
}

/// Check a downloaded template package against its index entry.
pub fn verify_template(entry: &TemplateEntry, bytes: &[u8]) -> Result<()> {
    verify_digest(&entry.id, entry.size, &entry.sha256, bytes)
}

fn verify_digest(id: &str, size: u64, sha256: &str, bytes: &[u8]) -> Result<()> {
    if size != 0 && bytes.len() as u64 != size {
        return Err(PluginError::Registry(format!(
            "`{id}`: size {} does not match the index ({size})",
            bytes.len(),
        )));
    }
    let got = sha256_hex(bytes);
    if !got.eq_ignore_ascii_case(sha256) {
        return Err(PluginError::Registry(format!(
            "`{id}`: checksum mismatch (expected {sha256}, got {got})"
        )));
    }
    Ok(())
}

/// Download the index.
pub async fn fetch_index(url: &str) -> Result<RegistryIndex> {
    let text = reqwest::get(url)
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| PluginError::Registry(format!("index: {e}")))?
        .text()
        .await
        .map_err(|e| PluginError::Registry(e.to_string()))?;
    RegistryIndex::parse(&text)
}

/// Download an artifact and verify it. The caller then hands the bytes to
/// [`PluginStore::install_archive`](crate::store::PluginStore::install_archive).
pub async fn download(entry: &RegistryEntry) -> Result<Vec<u8>> {
    let bytes = fetch_artifact(&entry.download_url).await?;
    verify(entry, &bytes)?;
    Ok(bytes)
}

/// Download a template package, verify its hash and check it matches the entry.
pub async fn download_template(entry: &TemplateEntry) -> Result<TemplatePackage> {
    let bytes = fetch_artifact(&entry.download_url).await?;
    verify_template(entry, &bytes)?;
    let pkg = TemplatePackage::from_archive(&bytes)?;
    pkg.check_entry(entry)?;
    Ok(pkg)
}

async fn fetch_artifact(url: &str) -> Result<Vec<u8>> {
    let resp = reqwest::get(url)
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| PluginError::Registry(format!("download: {e}")))?;
    if resp
        .content_length()
        .is_some_and(|n| n > MAX_ARTIFACT_BYTES)
    {
        return Err(PluginError::Registry("artifact is too large".into()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| PluginError::Registry(e.to_string()))?;
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(PluginError::Registry("artifact is too large".into()));
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sha: &str) -> String {
        format!(
            r#"{{"version":1,"plugins":[{{"id":"dev.example.demo","name":"Demo","version":"1.2.0","download_url":"https://example.com/d.tar.gz","sha256":"{sha}","size":5,"capabilities":{{"http":["api.example.com"]}}}}]}}"#
        )
    }

    #[test]
    fn parses_and_validates_the_index() {
        let sha = sha256_hex(b"hello");
        let idx = RegistryIndex::parse(&entry(&sha)).unwrap();
        assert_eq!(
            idx.find("dev.example.demo").unwrap().capabilities.http,
            ["api.example.com"]
        );
        assert!(RegistryIndex::parse(&entry("zz")).is_err(), "bad sha");
        assert!(
            RegistryIndex::parse(&entry(&sha).replace("https://example.com", "http://example.com"))
                .is_err(),
            "http download"
        );
        assert!(
            RegistryIndex::parse(&entry(&sha).replace("\"version\":1,", "\"version\":2,")).is_err()
        );
        let dup =
            entry(&sha).replace("]}", ",") + &entry(&sha)[entry(&sha).find("{\"id\"").unwrap()..];
        assert!(RegistryIndex::parse(&dup).is_err(), "duplicate ids");
    }

    #[test]
    fn verifies_downloads() {
        let idx = RegistryIndex::parse(&entry(&sha256_hex(b"hello"))).unwrap();
        let e = idx.find("dev.example.demo").unwrap();
        assert!(verify(e, b"hello").is_ok());
        assert!(verify(e, b"hellp").is_err(), "checksum");
        assert!(verify(e, b"hello!").is_err(), "size");
    }

    #[test]
    fn version_ordering_and_updates() {
        assert!(is_newer("1.10.0", "1.9.9"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.9.0", "1.0.0"));
        assert!(!is_newer("x", "1.0.0"));
        let idx = RegistryIndex::parse(&entry(&sha256_hex(b"hello"))).unwrap();
        let installed = vec![
            ("dev.example.demo".to_string(), "1.0.0".to_string()),
            ("other".to_string(), "9.9.9".to_string()),
        ];
        let up = idx.updates(&installed);
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].1, "1.0.0");
        assert!(
            idx.updates(&[("dev.example.demo".into(), "1.2.0".into())])
                .is_empty()
        );
    }
}
