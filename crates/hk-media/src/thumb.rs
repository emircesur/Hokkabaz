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
        s("-hide_banner"),
        s("-nostdin"),
        s("-y"),
        s("-loglevel"),
        s("error"),
        s("-ss"),
        num(at),
        s("-i"),
        src.display().to_string(),
        s("-frames:v"),
        s("1"),
        s("-vf"),
        format!("scale={width}:-2"),
        s("-q:v"),
        s("3"),
        dst.display().to_string(),
    ]
}

/// Poster frame: ffmpeg's `thumbnail` filter picks the most representative frame in
/// `[start, start+dur)`; optional headline drawn from `text_file` (no escaping issues).
pub(crate) fn poster_args(
    src: &Path,
    start: f64,
    dur: f64,
    dst: &Path,
    width: u32,
    text_file: Option<&Path>,
) -> Vec<String> {
    let mut vf = format!("thumbnail=120,scale={width}:-2");
    if let Some(tf) = text_file {
        let path = tf
            .display()
            .to_string()
            .replace('\\', "/")
            .replace(':', "\\:")
            .replace('\'', "\\'");
        vf.push_str(&format!(
            ",drawtext=textfile='{path}':fontsize=h/9:fontcolor=white:borderw=h/90:bordercolor=black:line_spacing=h/60:x=(w-text_w)/2:y=h*0.70-text_h/2"
        ));
    }
    vec![
        s("-hide_banner"),
        s("-nostdin"),
        s("-y"),
        s("-loglevel"),
        s("error"),
        s("-ss"),
        num(start),
        s("-t"),
        num(dur.max(0.5)),
        s("-i"),
        src.display().to_string(),
        s("-vf"),
        vf,
        s("-frames:v"),
        s("1"),
        s("-q:v"),
        s("2"),
        dst.display().to_string(),
    ]
}

pub(crate) fn proxy_args(src: &Path, dst: &Path, height: u32) -> Vec<String> {
    vec![
        s("-hide_banner"),
        s("-nostdin"),
        s("-y"),
        s("-loglevel"),
        s("error"),
        s("-i"),
        src.display().to_string(),
        s("-vf"),
        format!("scale=-2:{height}"),
        s("-c:v"),
        s("libx264"),
        s("-preset"),
        s("veryfast"),
        s("-crf"),
        s("30"),
        // Short GOP keeps scrubbing/seeking responsive in the player.
        s("-g"),
        s("15"),
        s("-pix_fmt"),
        s("yuv420p"),
        s("-c:a"),
        s("aac"),
        s("-b:a"),
        s("64k"),
        s("-movflags"),
        s("+faststart"),
        dst.display().to_string(),
    ]
}

impl Tools {
    /// Grab one frame at `at` seconds, scaled to `width` (height keeps aspect). Format from `dst` extension.
    pub async fn thumbnail(&self, src: &Path, at: f64, dst: &Path, width: u32) -> Result<()> {
        run_capture(&self.ffmpeg, &thumbnail_args(src, at, dst, width)).await?;
        Ok(())
    }

    /// Poster/thumbnail: best frame in a window, optional headline (wrapped at ~18 chars/line).
    pub async fn poster(
        &self,
        src: &Path,
        start: f64,
        dur: f64,
        dst: &Path,
        width: u32,
        text: Option<&str>,
    ) -> Result<()> {
        let tf = match text.filter(|t| !t.trim().is_empty()) {
            Some(t) => {
                let p = dst.with_extension("txt");
                std::fs::write(&p, wrap(t, 18))?;
                Some(p)
            }
            None => None,
        };
        let r = run_capture(
            &self.ffmpeg,
            &poster_args(src, start, dur, dst, width, tf.as_deref()),
        )
        .await;
        if let Some(p) = tf {
            let _ = std::fs::remove_file(p);
        }
        r.map(drop)
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
        assert!(
            proxy_args(Path::new("a.mp4"), Path::new("p.mp4"), 360)
                .contains(&"scale=-2:360".to_string())
        );
    }
}

/// Greedy word wrap.
pub(crate) fn wrap(text: &str, width: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    for w in text.split_whitespace() {
        match lines.last_mut() {
            Some(l) if l.chars().count() + 1 + w.chars().count() <= width => {
                l.push(' ');
                l.push_str(w);
            }
            _ => lines.push(w.to_string()),
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod poster_tests {
    use super::*;

    #[test]
    fn poster_filter_and_wrap() {
        let a = poster_args(
            Path::new("in.mp4"),
            3.0,
            10.0,
            Path::new("o.jpg"),
            1280,
            Some(Path::new("C:/t.txt")),
        );
        let vf = &a[a.iter().position(|x| x == "-vf").unwrap() + 1];
        assert!(vf.starts_with("thumbnail=120,scale=1280:-2,drawtext=textfile='C\\:/t.txt'"));
        assert_eq!(
            wrap("this is a very long headline here", 10),
            "this is a\nvery long\nheadline\nhere"
        );
    }
}
