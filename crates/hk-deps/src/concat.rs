//! The Concat editor: find it, or fetch the platform's release from GitHub.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::{CONCAT, DepStatus, Deps, DepsError, Result, download, run_version};

const LATEST: &str = "https://api.github.com/repos/jub0t/Concat/releases/latest";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

/// The release asset to use on `os` / `arch`, if any. Names follow `Concat-<ver>-<platform>.<ext>`;
/// matching is by extension and architecture words so a rename of the version part is harmless.
pub fn pick_concat_asset<'a>(assets: &'a [Asset], os: &str, arch: &str) -> Option<&'a Asset> {
    let has = |a: &Asset, w: &str| a.name.to_ascii_lowercase().contains(w);
    let ends = |a: &Asset, e: &str| a.name.to_ascii_lowercase().ends_with(e);
    let arch_ok = |a: &Asset| {
        if arch == "aarch64" {
            has(a, "aarch64") || has(a, "arm64")
        } else {
            has(a, "x86_64")
                || has(a, "x64")
                || has(a, "amd64")
                || !(has(a, "aarch64") || has(a, "arm64"))
        }
    };
    let find = |f: &dyn Fn(&Asset) -> bool| assets.iter().find(|a| f(a));
    match os {
        "linux" => find(&|a| ends(a, ".appimage") && arch_ok(a)),
        "windows" => find(&|a| has(a, "windows") && ends(a, "setup.exe") && arch_ok(a))
            .or_else(|| find(&|a| has(a, "windows") && ends(a, ".msi") && arch_ok(a)))
            .or_else(|| find(&|a| has(a, "windows") && ends(a, ".exe"))),
        "macos" => find(&|a| ends(a, ".dmg") && has(a, "macos") && arch_ok(a))
            .or_else(|| find(&|a| ends(a, ".dmg") && has(a, "macos"))),
        _ => None,
    }
}

fn exe_in_app(app: &Path) -> PathBuf {
    app.join("Contents/MacOS/concat")
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// App-managed installs, then whatever `hk-concat` locates (config-free: env, PATH, known dirs).
pub(crate) fn find(tools: &Path) -> Option<PathBuf> {
    let managed = tools.join("concat").join(if cfg!(windows) {
        "concat.exe"
    } else {
        "Concat.AppImage"
    });
    if managed.is_file() {
        return Some(managed);
    }
    if cfg!(target_os = "macos")
        && let Some(h) = home()
    {
        let m = exe_in_app(&h.join("Applications/Concat.app"));
        if m.is_file() {
            return Some(m);
        }
    }
    hk_concat::locate_binary(None).ok()
}

pub(crate) async fn status(d: &Deps) -> DepStatus {
    let path = find(d.tools_dir());
    let version = match &path {
        // an AppImage may need FUSE just to print its version; absence of a version is not absence of Concat
        Some(p) => run_version(p, "--version").await,
        None => None,
    };
    let j = d.job_view(CONCAT);
    let supported = matches!(std::env::consts::OS, "linux" | "windows" | "macos");
    let note = if path.is_some() {
        "Fine editing: open any clip in Concat and render it there.".to_owned()
    } else if supported {
        "Not installed. Download the editor from its GitHub release.".to_owned()
    } else {
        "Concat runs on desktop. Connect to a desktop instance that has it.".to_owned()
    };
    DepStatus {
        id: CONCAT,
        name: "Concat editor",
        installed: path.is_some(),
        source: path.as_ref().map(|p| {
            if p.starts_with(d.tools_dir()) {
                "managed"
            } else {
                "system"
            }
        }),
        path: path.map(|p| p.display().to_string()),
        version,
        can_install: supported,
        note: j.notice.unwrap_or(note),
        installing: j.installing,
        phase: j.phase,
        progress: j.progress,
        error: j.error,
    }
}

pub(crate) async fn install(d: &Deps, cancel: &CancellationToken) -> Result<Option<String>> {
    let os = std::env::consts::OS;
    let client = download::client()?;
    d.set_phase(CONCAT, "looking up the latest release");
    let rel: Release = serde_json::from_str(&download::text(&client, LATEST).await?)
        .map_err(|e| DepsError::Download(format!("release info: {e}")))?;
    let asset = pick_concat_asset(&rel.assets, os, std::env::consts::ARCH)
        .ok_or_else(|| {
            DepsError::Unsupported(format!(
                "release {} has no download for {os}/{}; get it from github.com/jub0t/Concat/releases",
                rel.tag_name,
                std::env::consts::ARCH
            ))
        })?
        .clone();
    let dir = d.tools_dir().join("concat");
    tokio::fs::create_dir_all(&dir).await?;
    let dest = dir.join(&asset.name);
    d.set_phase(CONCAT, &format!("downloading {}", rel.tag_name));
    let dd = d.clone();
    download::file(&client, &asset.url, &dest, cancel, move |a, b| {
        dd.set_progress(CONCAT, a, b)
    })
    .await?;
    match os {
        "linux" => {
            let target = dir.join("Concat.AppImage");
            crate::replace::replace_file(&dest, &target)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                tokio::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).await?;
            }
            Ok(Some(format!(
                "Concat {} installed. AppImages need FUSE; without it run Concat from your package manager.",
                rel.tag_name
            )))
        }
        "windows" => {
            d.set_phase(CONCAT, "starting the installer");
            let lower = asset.name.to_ascii_lowercase();
            let mut cmd = if lower.ends_with(".msi") {
                let mut c = tokio::process::Command::new("msiexec");
                c.arg("/i").arg(&dest);
                c
            } else {
                tokio::process::Command::new(&dest)
            };
            cmd.spawn()
                .map_err(|e| DepsError::Unpack(format!("cannot start the installer: {e}")))?;
            Ok(Some(
                "The Concat installer is open. Finish it, then refresh this list.".into(),
            ))
        }
        "macos" => {
            d.set_phase(CONCAT, "installing");
            install_dmg(&dest, &dir).await?;
            let _ = tokio::fs::remove_file(&dest).await;
            Ok(Some(format!(
                "Concat {} installed in ~/Applications.",
                rel.tag_name
            )))
        }
        other => Err(DepsError::Unsupported(format!(
            "no Concat installer for {other}"
        ))),
    }
}

/// Mount the dmg, copy `Concat.app` to `~/Applications`, unmount.
async fn install_dmg(dmg: &Path, work: &Path) -> Result<()> {
    let mount = work.join("mnt");
    tokio::fs::create_dir_all(&mount).await?;
    let run = |prog: &'static str, args: Vec<std::ffi::OsString>| async move {
        let out = tokio::process::Command::new(prog)
            .args(args)
            .output()
            .await?;
        if out.status.success() {
            Ok(())
        } else {
            Err(DepsError::Unpack(format!(
                "{prog} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    };
    run(
        "hdiutil",
        vec![
            "attach".into(),
            "-nobrowse".into(),
            "-readonly".into(),
            "-mountpoint".into(),
            mount.clone().into(),
            dmg.into(),
        ],
    )
    .await?;
    let apps = home()
        .ok_or_else(|| DepsError::Unpack("no home directory".into()))?
        .join("Applications");
    let copy = async {
        tokio::fs::create_dir_all(&apps).await?;
        let _ = tokio::fs::remove_dir_all(apps.join("Concat.app")).await;
        // `ditto` keeps the code signature and extended attributes that `cp -R` can drop
        run(
            "ditto",
            vec![
                mount.join("Concat.app").into(),
                apps.join("Concat.app").into(),
            ],
        )
        .await
    }
    .await;
    let _ = run("hdiutil", vec!["detach".into(), mount.into()]).await;
    copy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets(names: &[&str]) -> Vec<Asset> {
        names
            .iter()
            .map(|n| Asset {
                name: (*n).into(),
                url: format!("https://x/{n}"),
                size: 1,
            })
            .collect()
    }

    #[test]
    fn picks_by_platform_and_arch() {
        let a = assets(&[
            "Concat-0.2.8-aarch64.AppImage",
            "Concat-0.2.8-x86_64.AppImage",
            "Concat-0.2.8-linux-x86_64.deb",
            "Concat-0.2.8-macos-arm64.dmg",
            "Concat-0.2.8-macos-x86_64.dmg",
            "Concat-0.2.8-windows-x86_64-setup.exe",
            "Concat-0.2.8-windows-x86_64.msi",
            "Concat-0.2.8-android-arm64.apk",
        ]);
        let n = |os, arch| pick_concat_asset(&a, os, arch).map(|x| x.name.as_str());
        assert_eq!(n("linux", "x86_64"), Some("Concat-0.2.8-x86_64.AppImage"));
        assert_eq!(n("linux", "aarch64"), Some("Concat-0.2.8-aarch64.AppImage"));
        assert_eq!(n("macos", "aarch64"), Some("Concat-0.2.8-macos-arm64.dmg"));
        assert_eq!(n("macos", "x86_64"), Some("Concat-0.2.8-macos-x86_64.dmg"));
        assert_eq!(
            n("windows", "x86_64"),
            Some("Concat-0.2.8-windows-x86_64-setup.exe")
        );
        assert_eq!(n("android", "aarch64"), None);
    }

    #[test]
    fn linux_without_a_matching_appimage_is_none() {
        let a = assets(&[
            "Concat-0.2.8-aarch64.AppImage",
            "Concat-0.2.8-linux-x86_64.deb",
        ]);
        assert!(pick_concat_asset(&a, "linux", "x86_64").is_none());
    }
}
