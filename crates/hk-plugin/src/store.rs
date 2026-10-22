//! Installed plugins: `<root>/<id>/{plugin.toml, <wasm>}` plus a `disabled` marker.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::{PluginError, Result};
use crate::manifest::Manifest;

const MAX_WASM_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone)]
pub struct InstalledPlugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub enabled: bool,
}

impl InstalledPlugin {
    pub fn wasm_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.wasm)
    }
}

#[derive(Debug, Clone)]
pub struct PluginStore {
    root: PathBuf,
}

fn is_wasm(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\0asm")
}

impl PluginStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir_of(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// Per-plugin data directory (key/value store).
    pub fn data_dir(&self, id: &str) -> PathBuf {
        self.root.join(".data").join(id)
    }

    pub fn list(&self) -> Result<Vec<InstalledPlugin>> {
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(&self.root) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for e in rd.flatten() {
            let dir = e.path();
            if !dir.is_dir() || e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            match Manifest::load(&dir) {
                Ok(manifest) => out.push(InstalledPlugin {
                    enabled: !dir.join("disabled").exists(),
                    manifest,
                    dir,
                }),
                Err(err) => {
                    tracing::warn!(dir = %dir.display(), error = %err, "skipping plugin with bad manifest")
                }
            }
        }
        out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        Ok(out)
    }

    pub fn get(&self, id: &str) -> Result<InstalledPlugin> {
        self.list()?
            .into_iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| PluginError::NotFound(format!("plugin {id}")))
    }

    /// Install from a directory containing `plugin.toml` and its wasm file.
    pub fn install_dir(&self, src: &Path) -> Result<InstalledPlugin> {
        let manifest = Manifest::load(src)?;
        let wasm = std::fs::read(src.join(&manifest.wasm))?;
        self.install_parts(
            &manifest,
            &std::fs::read_to_string(src.join("plugin.toml"))?,
            &wasm,
        )
    }

    /// Install from a `.tar.gz` holding `plugin.toml` and the wasm file (nothing else is read).
    pub fn install_archive(&self, bytes: &[u8]) -> Result<InstalledPlugin> {
        let gz = flate2::read::GzDecoder::new(bytes);
        let mut tar = tar::Archive::new(gz);
        let mut manifest_text: Option<String> = None;
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        for entry in tar
            .entries()
            .map_err(|e| PluginError::Registry(format!("bad archive: {e}")))?
        {
            let entry = entry.map_err(|e| PluginError::Registry(format!("bad archive: {e}")))?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry
                .path()
                .map_err(|e| PluginError::Registry(e.to_string()))?
                .to_path_buf();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            // Flat names only: no directories, no traversal; unknown files are ignored.
            let flat = path
                .components()
                .filter(|c| !matches!(c, std::path::Component::CurDir))
                .count()
                == 1;
            if !flat {
                continue;
            }
            let limit = if name == "plugin.toml" {
                MAX_MANIFEST_BYTES
            } else {
                MAX_WASM_BYTES
            };
            if name == "plugin.toml" || name.ends_with(".wasm") {
                let mut buf = Vec::new();
                entry.take(limit + 1).read_to_end(&mut buf)?;
                if buf.len() as u64 > limit {
                    return Err(PluginError::Registry(format!("`{name}` is too large")));
                }
                if name == "plugin.toml" {
                    manifest_text =
                        Some(String::from_utf8(buf).map_err(|_| {
                            PluginError::Manifest("plugin.toml is not UTF-8".into())
                        })?);
                } else {
                    files.push((name, buf));
                }
            }
        }
        let text = manifest_text
            .ok_or_else(|| PluginError::Registry("archive has no plugin.toml".into()))?;
        let manifest = Manifest::parse(&text)?;
        let wasm = files
            .into_iter()
            .find(|(n, _)| *n == manifest.wasm)
            .map(|(_, b)| b)
            .ok_or_else(|| PluginError::Registry(format!("archive has no `{}`", manifest.wasm)))?;
        self.install_parts(&manifest, &text, &wasm)
    }

    fn install_parts(
        &self,
        manifest: &Manifest,
        manifest_text: &str,
        wasm: &[u8],
    ) -> Result<InstalledPlugin> {
        if !is_wasm(wasm) {
            return Err(PluginError::Manifest(format!(
                "`{}` is not a WebAssembly file",
                manifest.wasm
            )));
        }
        if wasm.len() as u64 > MAX_WASM_BYTES {
            return Err(PluginError::Manifest("wasm file is too large".into()));
        }
        let dir = self.dir_of(&manifest.id);
        let was_disabled = dir.join("disabled").exists();
        let tmp = self.root.join(format!(".installing-{}", manifest.id));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        std::fs::write(tmp.join("plugin.toml"), manifest_text)?;
        std::fs::write(tmp.join(&manifest.wasm), wasm)?;
        if was_disabled {
            std::fs::write(tmp.join("disabled"), b"")?;
        }
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(&tmp, &dir)?;
        Ok(InstalledPlugin {
            manifest: manifest.clone(),
            enabled: !was_disabled,
            dir,
        })
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let p = self.get(id)?;
        std::fs::remove_dir_all(&p.dir)?;
        let _ = std::fs::remove_dir_all(self.data_dir(id));
        Ok(())
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let p = self.get(id)?;
        let marker = p.dir.join("disabled");
        if enabled {
            let _ = std::fs::remove_file(marker);
        } else {
            std::fs::write(marker, b"")?;
        }
        Ok(())
    }
}

/// Read just the manifest out of a plugin archive (for building registry entries).
pub fn archive_manifest(bytes: &[u8]) -> Result<Manifest> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    for entry in tar
        .entries()
        .map_err(|e| PluginError::Registry(format!("bad archive: {e}")))?
    {
        let entry = entry.map_err(|e| PluginError::Registry(format!("bad archive: {e}")))?;
        let is_manifest = entry
            .path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n == "plugin.toml"))
            .unwrap_or(false);
        if is_manifest {
            let mut text = String::new();
            entry.take(MAX_MANIFEST_BYTES).read_to_string(&mut text)?;
            return Manifest::parse(&text);
        }
    }
    Err(PluginError::Registry("archive has no plugin.toml".into()))
}

/// Pack a plugin directory (`plugin.toml` + the wasm it names) into the `.tar.gz` that
/// [`PluginStore::install_archive`] and the registry use. Validates the manifest first.
pub fn pack_dir(dir: &Path) -> Result<Vec<u8>> {
    let manifest_path = dir.join("plugin.toml");
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| PluginError::Manifest(format!("{}: {e}", manifest_path.display())))?;
    let manifest = Manifest::parse(
        std::str::from_utf8(&manifest_bytes).map_err(|e| PluginError::Manifest(e.to_string()))?,
    )?;
    let wasm = std::fs::read(dir.join(&manifest.wasm))
        .map_err(|e| PluginError::Manifest(format!("{}: {e}", manifest.wasm)))?;
    let mut tar_bytes = Vec::new();
    {
        let mut b = tar::Builder::new(&mut tar_bytes);
        for (name, data) in [
            ("plugin.toml", manifest_bytes.as_slice()),
            (manifest.wasm.as_str(), wasm.as_slice()),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, data)?;
        }
        b.finish()?;
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &tar_bytes)?;
    Ok(gz.finish()?)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const MANIFEST: &str =
        "id = \"dev.example.demo\"\nname = \"Demo\"\nversion = \"1.0.0\"\nwasm = \"demo.wasm\"\n";
    pub const WASM: &[u8] = b"\0asm\x01\0\0\0rest";

    pub fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_bytes);
            for (name, data) in files {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, name, *data).unwrap();
            }
            b.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn install_list_disable_remove() {
        let dir = tempfile::tempdir().unwrap();
        let store = PluginStore::new(dir.path().join("plugins"));
        assert!(store.list().unwrap().is_empty());
        let p = store
            .install_archive(&archive(&[
                ("plugin.toml", MANIFEST.as_bytes()),
                ("demo.wasm", WASM),
                ("README.md", b"ignored"),
            ]))
            .unwrap();
        assert!(p.enabled && p.wasm_path().is_file());
        assert!(!p.dir.join("README.md").exists());
        assert_eq!(store.list().unwrap().len(), 1);
        store.set_enabled("dev.example.demo", false).unwrap();
        assert!(!store.get("dev.example.demo").unwrap().enabled);
        // Reinstall (upgrade) keeps the disabled state.
        let again = store
            .install_archive(&archive(&[
                ("plugin.toml", MANIFEST.replace("1.0.0", "1.1.0").as_bytes()),
                ("demo.wasm", WASM),
            ]))
            .unwrap();
        assert!(!again.enabled && again.manifest.version == "1.1.0");
        store.remove("dev.example.demo").unwrap();
        assert!(store.list().unwrap().is_empty());
        assert!(store.remove("dev.example.demo").is_err());
    }

    #[test]
    fn rejects_bad_archives() {
        let dir = tempfile::tempdir().unwrap();
        let store = PluginStore::new(dir.path());
        assert!(store.install_archive(b"not a gzip").is_err());
        assert!(
            store
                .install_archive(&archive(&[("demo.wasm", WASM)]))
                .is_err(),
            "no manifest"
        );
        assert!(
            store
                .install_archive(&archive(&[("plugin.toml", MANIFEST.as_bytes())]))
                .is_err(),
            "no wasm"
        );
        assert!(
            store
                .install_archive(&archive(&[
                    ("plugin.toml", MANIFEST.as_bytes()),
                    ("demo.wasm", b"MZ not wasm")
                ]))
                .is_err(),
            "not wasm"
        );
        // Nested paths are ignored, so the wasm is "missing".
        assert!(
            store
                .install_archive(&archive(&[
                    ("plugin.toml", MANIFEST.as_bytes()),
                    ("sub/demo.wasm", WASM)
                ]))
                .is_err()
        );
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn install_from_directory() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("plugin.toml"), MANIFEST).unwrap();
        std::fs::write(src.join("demo.wasm"), WASM).unwrap();
        let store = PluginStore::new(dir.path().join("p"));
        assert_eq!(store.install_dir(&src).unwrap().manifest.name, "Demo");
    }
}
