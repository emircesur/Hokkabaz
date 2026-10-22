//! Edit Image (`n8n-nodes-base.editImage`): resize, crop, rotate, blur and convert the image in a
//! binary property. Pure Rust (`image`): PNG, JPEG, GIF (first frame), WebP (lossless).

use std::io::Cursor;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat};
use serde_json::{Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::CredentialTypes;
use crate::describe::{Choice, NodeBuilder, Prop};
use crate::kit;

pub const NODE_TYPE: &str = "n8n-nodes-base.editImage";

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "Edit Image")
        .description("Resize, crop, rotate, blur or convert an image")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:image")
        .color("#553399")
        .property(
            Prop::options(
                "operation",
                "Operation",
                vec![
                    Choice::new("Resize", "resize"),
                    Choice::new("Crop", "crop"),
                    Choice::new("Rotate", "rotate"),
                    Choice::new("Blur", "blur"),
                    Choice::new("Convert Only", "information"),
                ],
            )
            .default("resize"),
        )
        .property(
            Prop::string("dataPropertyName", "Property Name")
                .default("data")
                .description("Binary property holding the image"),
        )
        .property(
            Prop::number("width", "Width")
                .default(100)
                .show("operation", &["resize", "crop"]),
        )
        .property(
            Prop::number("height", "Height")
                .default(100)
                .show("operation", &["resize", "crop"]),
        )
        .property(
            Prop::options(
                "resizeOption",
                "Resize Option",
                vec![
                    Choice::new("Maximum Area (keep aspect)", "maximumArea"),
                    Choice::new("Ignore Aspect Ratio", "ignoreAspectRatio"),
                    Choice::new("Only if Larger", "onlyIfLarger"),
                    Choice::new("Only if Smaller", "onlyIfSmaller"),
                    Choice::new("Percent", "percent"),
                ],
            )
            .default("maximumArea")
            .show("operation", &["resize"]),
        )
        .property(
            Prop::number("positionX", "Position X")
                .default(0)
                .show("operation", &["crop"]),
        )
        .property(
            Prop::number("positionY", "Position Y")
                .default(0)
                .show("operation", &["crop"]),
        )
        .property(
            Prop::number("rotate", "Rotate (degrees)")
                .default(90)
                .description("Multiples of 90")
                .show("operation", &["rotate"]),
        )
        .property(
            Prop::number("blur", "Blur Strength")
                .default(5)
                .show("operation", &["blur"]),
        )
        .property(Prop::collection(
            "options",
            "Options",
            vec![
                Prop::options(
                    "format",
                    "Format",
                    vec![
                        Choice::new("PNG", "png"),
                        Choice::new("JPEG", "jpeg"),
                        Choice::new("GIF", "gif"),
                        Choice::new("WebP", "webp"),
                    ],
                ),
                Prop::string("fileName", "File Name"),
            ],
        ))
        .build();
    kit::serve_versions(&mut d, &[1.0, 1.1]);
    d
}

fn format_of(name: &str) -> Option<(ImageFormat, &'static str, &'static str)> {
    Some(match name {
        "png" => (ImageFormat::Png, "image/png", "png"),
        "jpeg" | "jpg" => (ImageFormat::Jpeg, "image/jpeg", "jpg"),
        "gif" => (ImageFormat::Gif, "image/gif", "gif"),
        "webp" => (ImageFormat::WebP, "image/webp", "webp"),
        _ => return None,
    })
}

/// Target size for a resize. `None` = leave as is.
pub(crate) fn resized(
    opt: &str,
    (w, h): (u32, u32),
    (tw, th): (u32, u32),
) -> Option<(u32, u32, bool)> {
    let fit = || {
        let r = f64::min(f64::from(tw) / f64::from(w), f64::from(th) / f64::from(h));
        (
            ((f64::from(w) * r).round() as u32).max(1),
            ((f64::from(h) * r).round() as u32).max(1),
        )
    };
    match opt {
        "ignoreAspectRatio" => Some((tw.max(1), th.max(1), false)),
        "onlyIfLarger" => (w > tw || h > th).then(|| {
            let (a, b) = fit();
            (a, b, false)
        }),
        "onlyIfSmaller" => (w < tw && h < th).then(|| {
            let (a, b) = fit();
            (a, b, false)
        }),
        "percent" => Some((
            ((f64::from(w) * f64::from(tw) / 100.0).round() as u32).max(1),
            ((f64::from(h) * f64::from(tw) / 100.0).round() as u32).max(1),
            false,
        )),
        _ => {
            let (a, b) = fit();
            Some((a, b, false))
        }
    }
}

#[derive(Debug)]
struct EditImage {
    desc: NodeTypeDescription,
}

#[async_trait]
impl FlowNode for EditImage {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let n = ctx.input.first().map_or(0, Vec::len);
        let mut out = Vec::new();
        for i in 0..n {
            let res: Result<Vec<Item>, FlowError> = (|| {
                let prop = kit::str_param(ctx, &self.desc, "dataPropertyName", i)?;
                let prop = if prop.is_empty() {
                    "data".to_owned()
                } else {
                    prop
                };
                let file = kit::binary_file(ctx, &ctx.input[0][i], &prop)?;
                let img = image::load_from_memory(&file.data)
                    .map_err(|e| kit::node_err(ctx, format!("Cannot read the image: {e}")))?;
                let num = |k: &str, d: i64| {
                    ctx.eval_param_at(k, i)
                        .ok()
                        .flatten()
                        .and_then(|v| v.as_i64())
                        .unwrap_or(d)
                };
                let op = kit::str_param(ctx, &self.desc, "operation", i)?;
                let img: DynamicImage = match op.as_str() {
                    "resize" => {
                        let opt = kit::str_param(ctx, &self.desc, "resizeOption", i)?;
                        match resized(
                            &opt,
                            (img.width(), img.height()),
                            (
                                num("width", 100).max(1) as u32,
                                num("height", 100).max(1) as u32,
                            ),
                        ) {
                            Some((w, h, _)) => img.resize_exact(w, h, FilterType::Lanczos3),
                            None => img,
                        }
                    }
                    "crop" => {
                        let (x, y) = (
                            num("positionX", 0).max(0) as u32,
                            num("positionY", 0).max(0) as u32,
                        );
                        let (w, h) = (
                            num("width", 100).max(1) as u32,
                            num("height", 100).max(1) as u32,
                        );
                        if x >= img.width() || y >= img.height() {
                            return Err(kit::node_err(
                                ctx,
                                "The crop position is outside the image",
                            ));
                        }
                        img.crop_imm(x, y, w.min(img.width() - x), h.min(img.height() - y))
                    }
                    "rotate" => match num("rotate", 90).rem_euclid(360) {
                        90 => img.rotate90(),
                        180 => img.rotate180(),
                        270 => img.rotate270(),
                        0 => img,
                        _ => {
                            return Err(kit::node_err(
                                ctx,
                                "Rotation must be a multiple of 90 degrees",
                            ));
                        }
                    },
                    "blur" => img.blur(num("blur", 5).max(0) as f32),
                    _ => img,
                };
                let want = kit::field_str(ctx, "options.format", i)?
                    .filter(|f| !f.is_empty())
                    .or_else(|| file.mime.strip_prefix("image/").map(str::to_owned))
                    .unwrap_or_else(|| "png".to_owned());
                let (fmt, mime, ext) =
                    format_of(&want).unwrap_or((ImageFormat::Png, "image/png", "png"));
                let mut bytes = Vec::new();
                // JPEG has no alpha channel
                let img = if fmt == ImageFormat::Jpeg {
                    DynamicImage::ImageRgb8(img.to_rgb8())
                } else {
                    img
                };
                img.write_to(&mut Cursor::new(&mut bytes), fmt)
                    .map_err(|e| kit::node_err(ctx, format!("Cannot encode the image: {e}")))?;
                let stem = file
                    .name
                    .rsplit_once('.')
                    .map_or(file.name.as_str(), |(s, _)| s);
                let fname = kit::field_str(ctx, "options.fileName", i)?
                    .filter(|f| !f.is_empty())
                    .unwrap_or_else(|| format!("{stem}.{ext}"));
                let mut it = ctx.input[0][i].clone();
                let mut bin = hk_flow::BinaryData {
                    data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                    mime_type: mime.to_owned(),
                    file_name: Some(fname),
                    file_extension: Some(ext.to_owned()),
                    ..hk_flow::BinaryData::default()
                };
                bin.file_type = Some("image".into());
                it.binary
                    .get_or_insert_with(Default::default)
                    .insert(prop, bin);
                it.json.insert("width".into(), json!(img.width()));
                it.json.insert("height".into(), json!(img.height()));
                it.json.insert("fileSize".into(), json!(bytes.len()));
                let _ = Value::Null;
                Ok(vec![it])
            })();
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

/// Node into the shared registry. Features: `int-image`.
pub fn register(nodes: &mut NodeRegistry, _creds: &CredentialTypes) -> Result<(), FlowError> {
    nodes.register(Arc::new(EditImage {
        desc: description(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_rules() {
        assert_eq!(
            resized("maximumArea", (200, 100), (100, 100)),
            Some((100, 50, false))
        );
        assert_eq!(resized("onlyIfLarger", (50, 50), (100, 100)), None);
        assert_eq!(
            resized("onlyIfSmaller", (50, 50), (100, 100)),
            Some((100, 100, false))
        );
        assert_eq!(
            resized("percent", (200, 100), (50, 0)),
            Some((100, 50, false))
        );
        assert_eq!(
            resized("ignoreAspectRatio", (200, 100), (30, 40)),
            Some((30, 40, false))
        );
    }
}
