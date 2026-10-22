//! Single-frame thumbnails and low-res scrubbing proxies.
use std::path::Path;

use crate::error::Result;
use crate::fmt::num;
use crate::proc::run_capture;
use crate::tools::Tools;

fn s(v: &str) -> String {
    v.to_string()
}

pub(crate) fn thumbnail_args(src: &Path, at: f64, dst: &Path, width: u32) -> Vec<String> {
    vec![
        s("-hide_banner"), s("-nostdin"), s("-y"), s("-loglevel"), s("error"),
        s("-ss"), num(at),
        s("-i"), src.display().to_string(),
        s("-frames:v"), s("1"),
        s("-vf"), format!("scale={width}:-2"),
        s("-q:v"), s("3"),
        dst.display().to_string(),
    ]
}

pub(crate) fn proxy_args(src: &Path, dst: &Path, height: u32) -> Vec<String> {
    vec![
        s("-hide_banner"), s("-nostdin"), s("-y"), s("-loglevel"), s("error"),
        s("-i"), src.display().to_string(),
        s("-vf"), format!("scale=-2:{height}"),
        s("-c:v"), s("libx264"), s("-preset"), s("veryfast"), s("-crf"), s("30"),
        // Short GOP keeps scrubbing/seeking responsive in the player.
        s("-g"), s("15"), s("-pix_fmt"), s("yuv420p"),
        s("-c:a"), s("aac"), s("-b:a"), s("64k"),
        s("-movflags"), s("+faststart"),
        dst.display().to_string(),
    ]
}

impl Tools {
    /// Grab one frame at `at` seconds, scaled to `width` (height keeps aspect). Format from `dst` extension.
    pub async fn thumbnail(&self, src: &Path, at: f64, dst: &Path, width: u32) -> Result<()> {
        run_capture(&self.ffmpeg, &thumbnail_args(src, at, dst, width)).await?;
        Ok(())
    }

    /// Low-res h264 proxy of `height` px (even width auto) for editing/scrubbing.
    pub async fn proxy(&self, src: &Path, dst: &Path, height: u32) -> Result<()> {
        run_capture(&self.ffmpeg, &proxy_args(src, dst, height)).await?;
        Ok(())
    }
}

pub async fn thumbnail(src: &Path, at: f64, dst: &Path, width: u32) -> Result<()> {
    Tools::from_env().thumbnail(src, at, dst, width).await
}

pub async fn proxy(src: &Path, dst: &Path, height: u32) -> Result<()> {
    Tools::from_env().proxy(src, dst, height).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_seeks_before_input() {
        let a = thumbnail_args(Path::new("a.mp4"), 1.5, Path::new("t.jpg"), 320);
        let ss = a.iter().position(|x| x == "-ss").unwrap();
        assert_eq!(a[ss + 1], "1.5");
        assert!(ss < a.iter().position(|x| x == "-i").unwrap());
        assert!(a.contains(&"scale=320:-2".to_string()));
    }

    #[test]
    fn proxy_scales_by_height() {
        assert!(proxy_args(Path::new("a.mp4"), Path::new("p.mp4"), 360).contains(&"scale=-2:360".to_string()));
    }
}
