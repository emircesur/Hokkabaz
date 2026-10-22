//! `hk plugin ...`: scaffold, build, pack, install and manage WASM plugins.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use hk_plugin::manifest::Manifest;
use hk_plugin::registry::{self, RegistryEntry, RegistryIndex};
use hk_plugin::store::{PluginStore, archive_manifest, pack_dir};

use crate::paths::Paths;
use crate::term::out;

const TARGET: &str = "wasm32-wasip2";
const SDK_GIT: &str = "https://github.com/emircesur/Hokkabaz";

#[derive(Debug, Subcommand)]
pub enum PluginCmd {
    /// Create a new plugin project (Rust, wasm32-wasip2).
    New {
        /// Directory to create; also the default crate name.
        dir: PathBuf,
        /// Plugin id, reverse-DNS style (default: `dev.example.<dir>`).
        #[arg(long)]
        id: Option<String>,
        /// Use a local checkout of the SDK instead of the git dependency.
        #[arg(long)]
        sdk_path: Option<PathBuf>,
    },
    /// Build the plugin and write `dist/plugin.toml`, `dist/<name>.wasm` and `dist/<id>-<version>.tar.gz`.
    Build {
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Print the registry `index.json` entry for a built archive (paste it into the registry repo).
    IndexEntry {
        archive: PathBuf,
        /// Where the archive will be downloaded from (https, e.g. a GitHub Release asset).
        #[arg(long)]
        url: String,
    },
    /// Validate a registry `index.json`; with `--download`, fetch each artifact and check its hash and manifest.
    VerifyIndex {
        index: PathBuf,
        #[arg(long)]
        download: bool,
    },
    /// Install from a `.tar.gz` archive or a directory holding `plugin.toml` + wasm (e.g. `dist`).
    Install { path: PathBuf },
    /// List installed plugins.
    Ls,
    /// Uninstall a plugin.
    Remove { id: String },
    /// Enable a plugin.
    Enable { id: String },
    /// Disable a plugin (kept on disk).
    Disable { id: String },
}

fn store(paths: &Paths) -> PluginStore {
    PluginStore::new(paths.data.join("plugins"))
}

pub async fn run(paths: &Paths, cmd: PluginCmd) -> Result<()> {
    match cmd {
        PluginCmd::IndexEntry { archive, url } => {
            let bytes = std::fs::read(&archive).with_context(|| archive.display().to_string())?;
            let m = archive_manifest(&bytes)?;
            let entry = RegistryEntry {
                id: m.id,
                name: m.name,
                version: m.version,
                description: m.description,
                author: m.author,
                license: m.license,
                homepage: m.homepage,
                capabilities: m.capabilities,
                download_url: url,
                sha256: registry::sha256_hex(&bytes),
                size: bytes.len() as u64,
            };
            out(&format!("{}\n", serde_json::to_string_pretty(&entry)?));
            Ok(())
        }
        PluginCmd::VerifyIndex { index, download } => {
            let idx = RegistryIndex::parse(
                &std::fs::read_to_string(&index).with_context(|| index.display().to_string())?,
            )?;
            for e in &idx.plugins {
                if download {
                    let bytes = registry::download(e).await?;
                    let m = archive_manifest(&bytes)?;
                    if m.id != e.id || m.version != e.version || m.capabilities != e.capabilities {
                        bail!(
                            "{}: index entry does not match the archive's plugin.toml (id, version, capabilities)",
                            e.id
                        );
                    }
                }
                out(&format!("ok {} v{}\n", e.id, e.version));
            }
            Ok(())
        }
        PluginCmd::New { dir, id, sdk_path } => new(&dir, id, sdk_path),
        PluginCmd::Build { dir } => build(&dir),
        PluginCmd::Install { path } => {
            let s = store(paths);
            let p = if path.is_dir() {
                s.install_dir(&path)
            } else {
                s.install_archive(
                    &std::fs::read(&path).with_context(|| path.display().to_string())?,
                )
            }?;
            out(&format!(
                "installed {} v{}\n",
                p.manifest.id, p.manifest.version
            ));
            let caps = p.manifest.capability_summary();
            if caps.is_empty() {
                out("  access: none\n");
            }
            for c in caps {
                out(&format!("  access: {c}\n"));
            }
            Ok(())
        }
        PluginCmd::Ls => {
            for p in store(paths).list()? {
                out(&format!(
                    "{}\t{}\t{}\t{}\n",
                    p.manifest.id,
                    p.manifest.version,
                    if p.enabled { "enabled" } else { "disabled" },
                    p.manifest.name
                ));
            }
            Ok(())
        }
        PluginCmd::Remove { id } => Ok(store(paths).remove(&id)?),
        PluginCmd::Enable { id } => Ok(store(paths).set_enabled(&id, true)?),
        PluginCmd::Disable { id } => Ok(store(paths).set_enabled(&id, false)?),
    }
}

fn snake(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if out.chars().next().is_none_or(|c| c.is_ascii_digit()) {
        out.insert(0, 'p');
    }
    out
}

fn new(dir: &Path, id: Option<String>, sdk_path: Option<PathBuf>) -> Result<()> {
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }
    let base = dir
        .file_name()
        .and_then(|n| n.to_str())
        .context("directory needs a name")?;
    let crate_name = snake(base);
    let id = id.unwrap_or_else(|| {
        format!(
            "dev.example.{}",
            base.to_ascii_lowercase().replace(['_', ' '], "-")
        )
    });
    let sdk = match sdk_path {
        Some(p) => format!(
            "{{ path = {:?} }}",
            p.canonicalize()
                .with_context(|| p.display().to_string())?
                .display()
                .to_string()
        ),
        None => format!("{{ git = \"{SDK_GIT}\" }}"),
    };
    std::fs::create_dir_all(dir.join("src"))?;
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
             # Built for {TARGET}; keep this crate out of any parent workspace.\n[workspace]\n\n\
             [lib]\ncrate-type = [\"cdylib\"]\n\n[dependencies]\nhk-plugin-sdk = {sdk}\n\n\
             [profile.release]\nopt-level = \"s\"\nlto = true\nstrip = true\n"
        ),
    )?;
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "id = \"{id}\"\nname = \"{base}\"\nversion = \"0.1.0\"\ndescription = \"\"\nwasm = \"{crate_name}.wasm\"\nauthor = \"\"\nlicense = \"\"\n\n\
             # Deny-by-default. List only what the plugin needs; users see this before installing.\n[capabilities]\n\
             http = []      # hosts, e.g. [\"api.example.com\", \"*.example.org\"]\nkv = false\nsecrets = []   # secret names the plugin may read\nemit = false\n"
        ),
    )?;
    std::fs::write(
        dir.join("src/lib.rs"),
        r#"use hk_plugin_sdk::{Description, Plugin, ToolDecl, Value, export_plugin, json};

struct MyPlugin;

impl Plugin for MyPlugin {
    fn describe() -> Description {
        Description::new().tool(ToolDecl::new(
            "echo",
            "Echo the given text back.",
            json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
        ))
    }

    fn call_tool(name: &str, args: Value) -> Result<String, String> {
        match name {
            "echo" => Ok(args["text"].as_str().ok_or("`text` is required")?.to_owned()),
            _ => Err(format!("unknown tool `{name}`")),
        }
    }
}

export_plugin!(MyPlugin);
"#,
    )?;
    std::fs::write(dir.join(".gitignore"), "/target\n/dist\n")?;
    out(&format!(
        "created {}\n  next: cd {} && hk plugin build && hk plugin install dist\n",
        dir.display(),
        dir.display()
    ));
    Ok(())
}

fn build(dir: &Path) -> Result<()> {
    let manifest = Manifest::load(dir)?;
    let status = Command::new("cargo")
        .args(["build", "--release", "--target", TARGET])
        .current_dir(dir)
        .status()
        .context("run cargo (is the toolchain installed?)")?;
    if !status.success() {
        bail!("cargo build failed; if the target is missing: rustup target add {TARGET}");
    }
    let meta = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(dir)
        .output()?;
    let meta: serde_json::Value = serde_json::from_slice(&meta.stdout).context("cargo metadata")?;
    let target_dir = meta["target_directory"]
        .as_str()
        .context("target_directory")?;
    let lib = meta["packages"][0]["name"]
        .as_str()
        .context("package name")?
        .replace('-', "_");
    let built = Path::new(target_dir)
        .join(TARGET)
        .join("release")
        .join(format!("{lib}.wasm"));
    let dist = dir.join("dist");
    std::fs::create_dir_all(&dist)?;
    std::fs::copy(&built, dist.join(&manifest.wasm))
        .with_context(|| format!("copy {}", built.display()))?;
    std::fs::copy(dir.join("plugin.toml"), dist.join("plugin.toml"))?;
    let archive = dist.join(format!("{}-{}.tar.gz", manifest.id, manifest.version));
    std::fs::write(&archive, pack_dir(&dist)?)?;
    let sum = hk_plugin::registry::sha256_hex(&std::fs::read(&archive)?);
    out(&format!("built {}\n  sha256 {sum}\n", archive.display()));
    Ok(())
}
