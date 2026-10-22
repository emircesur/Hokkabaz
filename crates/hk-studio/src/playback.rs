use std::path::Path;

use hk_concat::{ConcatConfig, ConcatMode, ConcatSession};
use hk_media::Segment;
use serde_json::{Value, json};

use crate::store::atomic_json;
use crate::{Project, Result, Span, Studio, StudioError, invalid};

impl Studio {
    pub(crate) fn playback(
        &self,
        project: &Project,
        clip: Option<Span>,
        dir: &Path,
    ) -> Result<Value> {
        let key = self.render_key(project, clip)?;
        let ranges = project.kept_ranges(clip)?;
        let duration: f64 = ranges.iter().map(|r| r.duration()).sum();
        if duration <= 0.0 {
            return Err(invalid("Nothing remains after the selected cuts"));
        }
        let edl_path = dir.join(format!("playback-{key}.edl"));
        let source = project.source.path.to_string_lossy();
        // mpv's length quoting handles commas, semicolons, Unicode and Windows paths.
        let edl = format!(
            "# mpv EDL v0\n{}",
            ranges
                .iter()
                .map(|r| format!(
                    "%{}%{},{},{}\n",
                    source.len(),
                    source,
                    r.start,
                    r.duration()
                ))
                .collect::<String>()
        );
        std::fs::write(&edl_path, edl)?;
        let mut spec = self.media_spec(project, clip, dir, &format!("preview-{key}"), &edl_path)?;
        let (width, height) = project.edit.aspect.size();
        let factor = 640.0 / f64::from(width.max(height));
        let size = (
            (f64::from(width) * factor / 2.0).round() as u32 * 2,
            (f64::from(height) * factor / 2.0).round() as u32 * 2,
        );
        spec.size = Some(size);
        for overlay in &mut spec.overlays {
            overlay.width = Some(size.0);
        }
        // EDL does the seeking/cuts; the same export graph supplies every effect,
        // B-roll, libass caption and ducked music without producing a render file.
        spec.segments = vec![Segment {
            source: 0,
            start: 0.0,
            end: duration,
        }];
        Ok(
            json!({"path":edl_path,"filter":spec.playback_filter()?,"duration":duration,"ranges":ranges,"key":key,"size":size}),
        )
    }

    pub(crate) async fn prepare_editor(
        &self,
        project: &Project,
        args: &Value,
        dir: &Path,
    ) -> Result<Value> {
        let clip = self.clip(project, args)?.map(|c| c.span);
        let config = self.connections()?;
        let binary = hk_concat::locate_binary(config.concat_binary.as_deref())?;
        use sha2::{Digest, Sha256};
        let slot = format!(
            "{:x}",
            Sha256::digest(args["clip_id"].to_string().as_bytes())
        );
        let link = dir.join(format!("editor-{slot}.json"));
        let legacy = dir.join("editor.json");
        let previous = if link.is_file() { &link } else { &legacy };
        let key = self.render_key(project, clip)?;
        if args["rebuild"] != true && previous.is_file() {
            let mut saved: Value = serde_json::from_slice(&std::fs::read(previous)?)?;
            if saved["clip_id"] == args["clip_id"]
                && saved["path"]
                    .as_str()
                    .is_some_and(|p| Path::new(p).join("concat.json").is_file())
            {
                saved["binary"] = json!(binary);
                saved["shorts_changed"] = json!(saved["key"] != key);
                atomic_json(&link, &saved)?;
                return Ok(saved);
            }
        }
        let location = dir.join("editor");
        std::fs::create_dir_all(&location)?;
        let mut plan = self.concat_plan(project, clip)?;
        let name: String = project
            .name
            .chars()
            .filter(|c| c.is_alphanumeric() || "-_ ".contains(*c))
            .take(48)
            .collect();
        plan.name = format!(
            "{}-{}",
            if name.trim().is_empty() {
                "Studio"
            } else {
                name.trim()
            },
            &uuid::Uuid::new_v4().to_string()[..8]
        );
        let session = ConcatSession::new(ConcatConfig {
            path: Some(binary.clone()),
            mode: ConcatMode::Headless,
            roots: vec![
                dir.to_path_buf(),
                project.source.path.parent().unwrap_or(dir).to_path_buf(),
            ],
            ..ConcatConfig::default()
        });
        let result = async {
            let client = session.client().await?;
            let applied = hk_concat::apply_plan(&client, location.to_string_lossy().as_ref(), &plan).await?;
            client.project_save(&applied.project, None).await?;
            let data = json!({"path":applied.project,"binary":binary,"key":key,"clip_id":args["clip_id"],"revision":project.revision,"shorts_changed":false});
            atomic_json(&link, &data)?;
            atomic_json(&legacy, &data)?;
            Ok::<_, StudioError>(data)
        }.await;
        session.shutdown().await;
        result
    }
}
