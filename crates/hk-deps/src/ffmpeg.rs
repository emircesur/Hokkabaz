//! ffmpeg + ffprobe.

use std::path::PathBuf;

use tokio_util::sync::CancellationToken;

use crate::{DepStatus, Deps, DepsError, FFMPEG, Result, archive, download, run_version};

const BTBN: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest";

/// What to download on this platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfmpegBuild {
    /// One archive holding `bin/ffmpeg` and `bin/ffprobe`; `sums` lists its SHA-256.
    Archive {
        name: String,
        url: String,
        sums: String,
    },
    /// One single-binary zip per tool (macOS).
    PerTool { ffmpeg: String, ffprobe: String },
}

/// The build for `os` / `arch` (`std::env::consts` names), or why there is none.
pub fn ffmpeg_builds(os: &str, arch: &str) -> std::result::Result<FfmpegBuild, String> {
    let archive = |n: &str| FfmpegBuild::Archive {
        name: n.to_owned(),
        url: format!("{BTBN}/{n}"),
        sums: format!("{BTBN}/checksums.sha256"),
    };
    match (os, arch) {
        ("windows", "x86_64") => Ok(archive("ffmpeg-master-latest-win64-gpl.zip")),
        ("linux", "x86_64") => Ok(archive("ffmpeg-master-latest-linux64-gpl.tar.xz")),
        ("linux", "aarch64") => Ok(archive("ffmpeg-master-latest-linuxarm64-gpl.tar.xz")),
        ("macos", _) => Ok(FfmpegBuild::PerTool {
            ffmpeg: "https://evermeet.cx/ffmpeg/getrelease/ffmpeg/zip".into(),
            ffprobe: "https://evermeet.cx/ffmpeg/getrelease/ffprobe/zip".into(),
        }),
        ("android", _) | ("ios", _) => {
            Err("this device cannot run ffmpeg; connect to an instance that has it".into())
        }
        (o, a) => Err(format!(
            "no prebuilt ffmpeg for {o}/{a}; install it with your package manager"
        )),
    }
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

pub(crate) async fn status(d: &Deps) -> DepStatus {
    let t = hk_media::Tools::from_env();
    let (ff, fp) = (
        hk_media::resolve_program(&t.ffmpeg),
        hk_media::resolve_program(&t.ffprobe),
    );
    let (v_ff, v_fp) = (
        run_version(&ff, "-version").await,
        run_version(&fp, "-version").await,
    );
    let installed = v_ff.is_some() && v_fp.is_some();
    let managed = hk_media::managed_dir().is_some_and(|m| ff.starts_with(m));
    let build = ffmpeg_builds(std::env::consts::OS, std::env::consts::ARCH);
    let j = d.job_view(FFMPEG);
    let missing = match (&v_ff, &v_fp) {
        (None, None) => "ffmpeg and ffprobe",
        (None, _) => "ffmpeg",
        (_, None) => "ffprobe",
        _ => "",
    };
    let note = match (&build, installed) {
        (_, true) => "Cuts, renders, thumbnails and audio extraction.".into(),
        (Ok(_), false) => format!("{missing} not found. Download a static build (about 80 MB)."),
        (Err(why), false) => why.clone(),
    };
    DepStatus {
        id: FFMPEG,
        name: "ffmpeg",
        installed,
        path: installed.then(|| ff.display().to_string()),
        version: v_ff,
        source: installed.then_some(if managed { "managed" } else { "system" }),
        can_install: build.is_ok(),
        note: j.notice.unwrap_or(note),
        installing: j.installing,
        phase: j.phase,
        progress: j.progress,
        error: j.error,
    }
}

pub(crate) async fn install(d: &Deps, cancel: &CancellationToken) -> Result<Option<String>> {
    let build = ffmpeg_builds(std::env::consts::OS, std::env::consts::ARCH)
        .map_err(DepsError::Unsupported)?;
    let out_dir: PathBuf = d.tools_dir().join("ffmpeg");
    let work = d.tools_dir().join(".downloads");
    tokio::fs::create_dir_all(&out_dir).await?;
    crate::replace::cleanup_aside(&out_dir);
    let client = download::client()?;
    match build {
        FfmpegBuild::Archive { name, url, sums } => {
            let dest = work.join(&name);
            d.set_phase(FFMPEG, "downloading");
            let dd = d.clone();
            let got = download::file(&client, &url, &dest, cancel, move |a, b| {
                dd.set_progress(FFMPEG, a, b);
            })
            .await?;
            d.set_phase(FFMPEG, "verifying");
            // The host publishes sums next to the build; no sums file means no check (warned).
            match download::text(&client, &sums).await {
                Ok(t) => match download::sum_for(&t, &name) {
                    Some(want) if want != got => {
                        let _ = tokio::fs::remove_file(&dest).await;
                        return Err(DepsError::Checksum(name));
                    }
                    Some(_) => {}
                    None => tracing::warn!(%name, "no checksum listed; skipping verification"),
                },
                Err(e) => {
                    tracing::warn!(error = %e, "checksums unavailable; skipping verification")
                }
            }
            d.set_phase(FFMPEG, "unpacking");
            let names = [exe("ffmpeg"), exe("ffprobe")];
            let (dest2, out2) = (dest.clone(), out_dir.clone());
            tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                archive::extract_bins(&dest2, &refs, &out2)
            })
            .await
            .map_err(|e| DepsError::Unpack(e.to_string()))??;
            let _ = tokio::fs::remove_file(&dest).await;
        }
        FfmpegBuild::PerTool { ffmpeg, ffprobe } => {
            for (tool, url) in [("ffmpeg", ffmpeg), ("ffprobe", ffprobe)] {
                let dest = work.join(format!("{tool}.zip"));
                d.set_phase(FFMPEG, &format!("downloading {tool}"));
                let dd = d.clone();
                download::file(&client, &url, &dest, cancel, move |a, b| {
                    dd.set_progress(FFMPEG, a, b)
                })
                .await?;
                d.set_phase(FFMPEG, &format!("unpacking {tool}"));
                let (dest2, out2) = (dest.clone(), out_dir.join(tool));
                tokio::task::spawn_blocking(move || archive::extract_single(&dest2, &out2))
                    .await
                    .map_err(|e| DepsError::Unpack(e.to_string()))??;
                let _ = tokio::fs::remove_file(&dest).await;
            }
        }
    }
    d.set_phase(FFMPEG, "checking");
    let ok = run_version(&out_dir.join(exe("ffmpeg")), "-version")
        .await
        .is_some()
        && run_version(&out_dir.join(exe("ffprobe")), "-version")
            .await
            .is_some();
    if !ok {
        return Err(DepsError::Unpack(
            "the downloaded ffmpeg does not run on this machine (missing system libraries?)".into(),
        ));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_build_per_platform() {
        assert!(
            matches!(ffmpeg_builds("windows", "x86_64"), Ok(FfmpegBuild::Archive { name, .. }) if name.ends_with("win64-gpl.zip"))
        );
        assert!(
            matches!(ffmpeg_builds("linux", "aarch64"), Ok(FfmpegBuild::Archive { name, .. }) if name.contains("linuxarm64"))
        );
        assert!(matches!(
            ffmpeg_builds("macos", "aarch64"),
            Ok(FfmpegBuild::PerTool { .. })
        ));
        assert!(
            ffmpeg_builds("android", "aarch64")
                .unwrap_err()
                .contains("instance")
        );
        assert!(ffmpeg_builds("freebsd", "x86_64").is_err());
    }
}
