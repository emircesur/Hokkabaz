//! Brand kit (P9.36): logo/watermark, colors and font applied on top of any template.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::captions::CaptionStyle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Corner {
    TopLeft,
    #[default]
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrandKit {
    pub name: String,
    /// PNG with alpha recommended.
    #[serde(default)]
    pub logo: Option<PathBuf>,
    #[serde(default)]
    pub logo_corner: Corner,
    /// Logo width as a fraction of the output width.
    #[serde(default = "default_logo_width")]
    pub logo_width: f64,
    #[serde(default = "default_opacity")]
    pub logo_opacity: f64,
    /// Caption font family (installed on the render machine).
    #[serde(default)]
    pub font: Option<String>,
    /// `#RRGGBB` caption text color.
    #[serde(default)]
    pub primary_color: Option<String>,
    /// `#RRGGBB` caption highlight (karaoke word) color.
    #[serde(default)]
    pub accent_color: Option<String>,
    /// Clips played before / after every rendered clip. Stored for templates; not yet
    /// rendered (needs size-normalized concat).
    #[serde(default)]
    pub intro: Option<PathBuf>,
    #[serde(default)]
    pub outro: Option<PathBuf>,
}

fn default_logo_width() -> f64 {
    0.14
}
fn default_opacity() -> f64 {
    0.9
}

impl BrandKit {
    /// Brand font/colors over a caption style; unset brand fields keep the style's.
    pub fn style_captions(&self, s: &CaptionStyle) -> CaptionStyle {
        let mut s = s.clone();
        if let Some(f) = &self.font {
            s.font.clone_from(f);
        }
        if let Some(c) = &self.primary_color {
            s.primary.clone_from(c);
        }
        if let Some(c) = &self.accent_color {
            s.highlight.clone_from(c);
        }
        s
    }

    /// Logo overlay for an output of `size` lasting `duration` seconds.
    pub fn logo_overlay(&self, size: (u32, u32), duration: f64) -> Option<hk_media::Overlay> {
        let path = self.logo.clone()?;
        let w = ((f64::from(size.0) * self.logo_width.clamp(0.02, 0.5)) as u32).max(16) & !1;
        let m = (f64::from(size.0.min(size.1)) * 0.04) as u32;
        let (x, y) = match self.logo_corner {
            Corner::TopLeft => (format!("{m}"), format!("{m}")),
            Corner::TopRight => (format!("W-w-{m}"), format!("{m}")),
            Corner::BottomLeft => (format!("{m}"), format!("H-h-{m}")),
            Corner::BottomRight => (format!("W-w-{m}"), format!("H-h-{m}")),
        };
        Some(hk_media::Overlay {
            path,
            kind: hk_media::OverlayKind::Image,
            start: 0.0,
            end: duration,
            x: Some(x),
            y: Some(y),
            width: Some(w),
            opacity: Some(self.logo_opacity.clamp(0.0, 1.0)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_and_colors() {
        let b = BrandKit {
            name: "acme".into(),
            logo: Some("logo.png".into()),
            logo_corner: Corner::BottomRight,
            logo_width: 0.1,
            logo_opacity: 1.0,
            font: Some("Inter".into()),
            primary_color: None,
            accent_color: Some("#F2A33A".into()),
            intro: None,
            outro: None,
        };
        let o = b.logo_overlay((1080, 1920), 12.0).unwrap();
        assert_eq!(
            (o.width, o.x.as_deref(), o.y.as_deref()),
            (Some(108), Some("W-w-43"), Some("H-h-43"))
        );
        let s = b.style_captions(&CaptionStyle::classic());
        assert_eq!(
            (s.font.as_str(), s.highlight.as_str()),
            ("Inter", "#F2A33A")
        );
        assert_eq!(s.primary, CaptionStyle::classic().primary);
    }
}
