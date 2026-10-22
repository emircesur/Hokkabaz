//! Pull named executables out of a `.zip` / `.tar.xz` without unpacking the whole archive.

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use crate::{DepsError, Result};

fn unpack_err(e: impl std::fmt::Display) -> DepsError {
    DepsError::Unpack(e.to_string())
}

/// True when archive path `entry` is `want` (a file name) directly inside a `bin/` folder.
fn is_bin(entry: &str, want: &str) -> bool {
    let e = entry.replace('\\', "/");
    e.ends_with(&format!("/bin/{want}")) || e == format!("bin/{want}")
}

fn write_exe(dest: &Path, r: &mut impl Read) -> Result<()> {
    let tmp = dest.with_extension("part");
    {
        let mut f = std::fs::File::create(&tmp)?;
        std::io::copy(r, &mut f)?;
        f.flush()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// Extract each of `names` (found under a `bin/` folder) into `out_dir`. All must be present.
pub(crate) fn extract_bins(archive: &Path, names: &[&str], out_dir: &Path) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out_dir)?;
    let lower = archive.to_string_lossy().to_ascii_lowercase();
    let mut got: Vec<PathBuf> = Vec::new();
    if lower.ends_with(".zip") {
        let mut z = zip::ZipArchive::new(BufReader::new(std::fs::File::open(archive)?))
            .map_err(unpack_err)?;
        for i in 0..z.len() {
            let mut e = z.by_index(i).map_err(unpack_err)?;
            if e.is_dir() {
                continue;
            }
            let name = e.name().to_owned();
            if let Some(w) = names.iter().find(|w| is_bin(&name, w)) {
                let dest = out_dir.join(w);
                write_exe(&dest, &mut e)?;
                got.push(dest);
            }
        }
    } else if lower.ends_with(".tar.xz") {
        // xz has no seek-friendly index: decompress to a sibling .tar, then walk it.
        let tar_path = archive.with_extension("tar");
        {
            let mut input = BufReader::new(std::fs::File::open(archive)?);
            let mut out = std::io::BufWriter::new(std::fs::File::create(&tar_path)?);
            lzma_rs::xz_decompress(&mut input, &mut out).map_err(unpack_err)?;
            out.flush()?;
        }
        let res = (|| -> Result<()> {
            let mut t = tar::Archive::new(BufReader::new(std::fs::File::open(&tar_path)?));
            for e in t.entries()? {
                let mut e = e?;
                let name = e.path()?.to_string_lossy().into_owned();
                if let Some(w) = names.iter().find(|w| is_bin(&name, w)) {
                    let dest = out_dir.join(w);
                    write_exe(&dest, &mut e)?;
                    got.push(dest);
                }
            }
            Ok(())
        })();
        let _ = std::fs::remove_file(&tar_path);
        res?;
    } else {
        return Err(DepsError::Unpack(format!(
            "unknown archive type: {}",
            archive.display()
        )));
    }
    for w in names {
        if !got.iter().any(|p| p.file_name().is_some_and(|n| n == *w)) {
            return Err(DepsError::Unpack(format!("{w} is not in the archive")));
        }
    }
    Ok(got)
}

/// Single-file zip (evermeet): the executable is the only entry.
pub(crate) fn extract_single(archive: &Path, dest: &Path) -> Result<()> {
    let mut z =
        zip::ZipArchive::new(BufReader::new(std::fs::File::open(archive)?)).map_err(unpack_err)?;
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(unpack_err)?;
        if !e.is_dir() {
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            return write_exe(dest, &mut e);
        }
    }
    Err(DepsError::Unpack("empty archive".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_with(path: &Path, files: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        for (n, b) in files {
            w.start_file(*n, o).unwrap();
            w.write_all(b).unwrap();
        }
        w.finish().unwrap();
    }

    fn tar_xz_with(path: &Path, files: &[(&str, &[u8])]) {
        let mut tar_bytes = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_bytes);
            for (n, data) in files {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, n, *data).unwrap();
            }
            b.finish().unwrap();
        }
        let mut out = std::fs::File::create(path).unwrap();
        lzma_rs::xz_compress(&mut std::io::Cursor::new(tar_bytes), &mut out).unwrap();
    }

    #[test]
    fn zip_takes_only_bin_entries() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("ff.zip");
        zip_with(
            &a,
            &[
                ("ffmpeg-x/bin/ffmpeg.exe", b"FF"),
                ("ffmpeg-x/bin/ffprobe.exe", b"FP"),
                ("ffmpeg-x/doc/ffmpeg.exe", b"WRONG"),
            ],
        );
        let out = d.path().join("out");
        extract_bins(&a, &["ffmpeg.exe", "ffprobe.exe"], &out).unwrap();
        assert_eq!(std::fs::read(out.join("ffmpeg.exe")).unwrap(), b"FF");
        assert_eq!(std::fs::read(out.join("ffprobe.exe")).unwrap(), b"FP");
    }

    #[test]
    fn tar_xz_roundtrip_and_missing_member() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("ff.tar.xz");
        tar_xz_with(
            &a,
            &[
                ("ffmpeg-n/bin/ffmpeg", b"FF"),
                ("ffmpeg-n/bin/ffprobe", b"FP"),
            ],
        );
        let out = d.path().join("out");
        extract_bins(&a, &["ffmpeg", "ffprobe"], &out).unwrap();
        assert_eq!(std::fs::read(out.join("ffprobe")).unwrap(), b"FP");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(out.join("ffmpeg"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0o111
            );
        }
        // the intermediate .tar is gone
        assert!(!d.path().join("ff.tar").exists());

        let b = d.path().join("bad.tar.xz");
        tar_xz_with(&b, &[("x/bin/ffmpeg", b"FF")]);
        assert!(extract_bins(&b, &["ffmpeg", "ffprobe"], &d.path().join("o2")).is_err());
    }

    #[test]
    fn single_file_zip() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("one.zip");
        zip_with(&a, &[("ffmpeg", b"BIN")]);
        extract_single(&a, &d.path().join("t/ffmpeg")).unwrap();
        assert_eq!(std::fs::read(d.path().join("t/ffmpeg")).unwrap(), b"BIN");
    }
}
