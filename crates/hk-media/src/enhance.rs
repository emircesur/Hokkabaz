//! Audio cleanup chains and hardware encoder selection for renders (P9.31, P9.37).

use serde::{Deserialize, Serialize};

use crate::fmt::num;

/// Noise reduction backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Denoise {
    /// `afftdn`; `nr` = reduction in dB (0.01..97), 12 is a sane default.
    Fft { nr: f64 },
    /// `arnndn` with an RNNoise model file (`.rnnn`).
    Rnnoise { model: String },
}

/// EBU R128 loudness target (`loudnorm`, single pass).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Loudness {
    /// Integrated loudness, LUFS. -14 for social platforms, -16 for podcasts.
    pub integrated: f64,
    pub true_peak: f64,
    pub lra: f64,
}

impl Default for Loudness {
    fn default() -> Self {
        Self {
            integrated: -14.0,
            true_peak: -1.5,
            lra: 11.0,
        }
    }
}

/// Voice cleanup applied to the program audio before music is mixed; loudness runs last.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioCleanup {
    pub highpass_hz: Option<u32>,
    pub denoise: Option<Denoise>,
    /// Gentle compression + presence boost for speech.
    pub voice_enhance: bool,
    pub loudness: Option<Loudness>,
}

impl AudioCleanup {
    /// Speech-focused preset: rumble cut, FFT denoise, enhance, -16 LUFS.
    pub fn podcast() -> Self {
        Self {
            highpass_hz: Some(80),
            denoise: Some(Denoise::Fft { nr: 12.0 }),
            voice_enhance: true,
            loudness: Some(Loudness {
                integrated: -16.0,
                ..Loudness::default()
            }),
        }
    }

    /// Social preset: light denoise, -14 LUFS.
    pub fn social() -> Self {
        Self {
            highpass_hz: Some(70),
            denoise: Some(Denoise::Fft { nr: 8.0 }),
            voice_enhance: false,
            loudness: Some(Loudness::default()),
        }
    }

    /// Filters for the voice track (before music), or `None` when nothing applies.
    pub fn voice_chain(&self) -> Option<String> {
        let mut f = Vec::new();
        if let Some(hz) = self.highpass_hz {
            f.push(format!("highpass=f={hz}"));
        }
        match &self.denoise {
            Some(Denoise::Fft { nr }) => {
                f.push(format!("afftdn=nr={}:nf=-25", num(nr.clamp(0.01, 97.0))))
            }
            Some(Denoise::Rnnoise { model }) => {
                f.push(format!("arnndn=m='{}'", model.replace('\'', "\\'")))
            }
            None => {}
        }
        if self.voice_enhance {
            f.push("acompressor=threshold=0.125:ratio=3:attack=10:release=200".into());
            f.push("equalizer=f=3000:t=q:w=1:g=3".into());
        }
        (!f.is_empty()).then(|| f.join(","))
    }

    /// Final loudness filter applied to the mixed output.
    pub fn master_chain(&self) -> Option<String> {
        self.loudness.map(|l| {
            format!(
                "loudnorm=I={}:TP={}:LRA={}",
                num(l.integrated),
                num(l.true_peak),
                num(l.lra)
            )
        })
    }
}

/// Video encoder family. `Software` uses libx264/libx265/libsvtav1.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum HwEncoder {
    #[default]
    Software,
    Nvenc,
    /// VAAPI render node, e.g. `/dev/dri/renderD128`.
    Vaapi {
        device: String,
    },
    Qsv,
    VideoToolbox,
    MediaCodec,
}

impl HwEncoder {
    /// ffmpeg encoder name for a codec, `None` when this family can't encode it.
    pub fn encoder_name(&self, codec: crate::VideoCodec) -> Option<&'static str> {
        use crate::VideoCodec::*;
        Some(match (self, codec) {
            (Self::Software, H264) => "libx264",
            (Self::Software, Hevc) => "libx265",
            (Self::Software, Av1) => "libsvtav1",
            (Self::Nvenc, H264) => "h264_nvenc",
            (Self::Nvenc, Hevc) => "hevc_nvenc",
            (Self::Nvenc, Av1) => "av1_nvenc",
            (Self::Vaapi { .. }, H264) => "h264_vaapi",
            (Self::Vaapi { .. }, Hevc) => "hevc_vaapi",
            (Self::Vaapi { .. }, Av1) => "av1_vaapi",
            (Self::Qsv, H264) => "h264_qsv",
            (Self::Qsv, Hevc) => "hevc_qsv",
            (Self::Qsv, Av1) => "av1_qsv",
            (Self::VideoToolbox, H264) => "h264_videotoolbox",
            (Self::VideoToolbox, Hevc) => "hevc_videotoolbox",
            (Self::MediaCodec, H264) => "h264_mediacodec",
            (Self::MediaCodec, Hevc) => "hevc_mediacodec",
            (Self::VideoToolbox | Self::MediaCodec, Av1) => return None,
        })
    }

    /// Args placed before the first `-i`.
    pub fn input_args(&self) -> Vec<String> {
        match self {
            Self::Vaapi { device } => vec!["-vaapi_device".into(), device.clone()],
            _ => Vec::new(),
        }
    }

    /// Filter appended to the final video label (upload to GPU surface).
    pub fn upload_filter(&self) -> Option<&'static str> {
        match self {
            Self::Vaapi { .. } => Some("format=nv12,hwupload"),
            _ => None,
        }
    }

    /// Default preset for the family, if it has one.
    pub fn default_preset(&self, codec: crate::VideoCodec) -> Option<&'static str> {
        use crate::VideoCodec::*;
        match (self, codec) {
            (Self::Software, Av1) => Some("8"),
            (Self::Software, _) => Some("medium"),
            (Self::Nvenc, _) => Some("p5"),
            (Self::Qsv, _) => Some("medium"),
            _ => None,
        }
    }

    /// Constant-quality args for this family.
    pub fn quality_args(&self, q: u8) -> Vec<String> {
        let q = q.to_string();
        match self {
            Self::Software => vec!["-crf".into(), q],
            Self::Nvenc => vec![
                "-rc".into(),
                "vbr".into(),
                "-cq".into(),
                q,
                "-b:v".into(),
                "0".into(),
            ],
            Self::Vaapi { .. } => vec!["-rc_mode".into(), "CQP".into(), "-qp".into(), q],
            Self::Qsv => vec!["-global_quality".into(), q],
            Self::VideoToolbox => {
                // VideoToolbox takes 1..100 quality, higher is better; map from CRF-like 0..51.
                let vt = (100u32.saturating_sub(u32::from(q.parse::<u8>().unwrap_or(23)) * 2))
                    .clamp(1, 100);
                vec!["-q:v".into(), vt.to_string()]
            }
            // MediaCodec has no CQ mode in ffmpeg; fall back to a bitrate.
            Self::MediaCodec => vec!["-b:v".into(), "8M".into()],
        }
    }

    /// Candidates to probe on this OS, best first.
    pub fn candidates() -> Vec<HwEncoder> {
        let mut v = Vec::new();
        if cfg!(target_os = "macos") {
            v.push(Self::VideoToolbox);
        } else if cfg!(target_os = "android") {
            v.push(Self::MediaCodec);
        } else {
            v.push(Self::Nvenc);
            v.push(Self::Qsv);
            if cfg!(target_os = "linux") {
                v.push(Self::Vaapi {
                    device: "/dev/dri/renderD128".into(),
                });
            }
        }
        v.push(Self::Software);
        v
    }

    /// Pick the first candidate whose encoder appears in `ffmpeg -encoders` output.
    pub fn pick(encoders_output: &str, codec: crate::VideoCodec) -> HwEncoder {
        Self::candidates()
            .into_iter()
            .find(|e| {
                e.encoder_name(codec).is_some_and(|n| {
                    *e == Self::Software || encoders_output.split_whitespace().any(|w| w == n)
                })
            })
            .unwrap_or_default()
    }
}

impl crate::Tools {
    /// Best available encoder family for `codec` (runs `ffmpeg -encoders`); software on failure.
    pub async fn detect_encoder(&self, codec: crate::VideoCodec) -> HwEncoder {
        let args = vec!["-hide_banner".to_string(), "-encoders".to_string()];
        match crate::proc::run_capture(&self.ffmpeg, &args).await {
            Ok(out) => HwEncoder::pick(&out.stdout, codec),
            Err(_) => HwEncoder::Software,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VideoCodec;

    #[test]
    fn podcast_chain() {
        let c = AudioCleanup::podcast();
        assert_eq!(
            c.voice_chain().unwrap(),
            "highpass=f=80,afftdn=nr=12:nf=-25,acompressor=threshold=0.125:ratio=3:attack=10:release=200,equalizer=f=3000:t=q:w=1:g=3"
        );
        assert_eq!(c.master_chain().unwrap(), "loudnorm=I=-16:TP=-1.5:LRA=11");
        assert!(AudioCleanup::default().voice_chain().is_none());
    }

    #[test]
    fn pick_encoder() {
        let out = " V....D h264_nvenc  NVIDIA NVENC H.264\n V....D libx264";
        let e = HwEncoder::pick(out, VideoCodec::H264);
        if cfg!(any(target_os = "macos", target_os = "android")) {
            assert_eq!(e, HwEncoder::Software);
        } else {
            assert_eq!(e, HwEncoder::Nvenc);
        }
        assert_eq!(
            HwEncoder::pick("libx264", VideoCodec::Hevc),
            HwEncoder::Software
        );
    }
}
