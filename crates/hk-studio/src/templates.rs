//! Templates: one-click bundles of caption style, reframing and cleanup options.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::captions::CaptionStyle;
use crate::error::Result;
use crate::project::{Clip, MusicSpec, SilenceCut};
use crate::reframe::ReframeSpec;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Template {
    pub name: String,
    pub description: String,
    pub captions: Option<CaptionStyle>,
    pub reframe: Option<ReframeSpec>,
    #[serde(default)]
    pub silence: Option<SilenceCut>,
    #[serde(default)]
    pub remove_fillers: bool,
    /// Ask the pipeline to add B-roll when the template is applied.
    #[serde(default)]
    pub broll: bool,
    #[serde(default)]
    pub music: Option<MusicSpec>,
}

pub fn builtin_templates() -> Vec<Template> {
    vec![
        Template {
            name: "shorts-karaoke".into(),
            description: "Vertical 9:16, big uppercase captions with the spoken word highlighted, tight cuts".into(),
            captions: Some(CaptionStyle::karaoke()),
            reframe: Some(ReframeSpec::vertical()),
            silence: Some(SilenceCut::default()),
            remove_fillers: true,
            broll: false,
            music: None,
        },
        Template {
            name: "shorts-pop".into(),
            description: "Vertical 9:16, one big word at a time, fast pacing".into(),
            captions: Some(CaptionStyle::pop()),
            reframe: Some(ReframeSpec::vertical()),
            silence: Some(SilenceCut { min_gap: 0.4, keep: 0.08 }),
            remove_fillers: true,
            broll: true,
            music: None,
        },
        Template {
            name: "clean-vertical".into(),
            description: "Vertical 9:16, classic lower captions, natural pacing".into(),
            captions: Some(CaptionStyle::classic()),
            reframe: Some(ReframeSpec::vertical()),
            silence: None,
            remove_fillers: false,
            broll: false,
            music: None,
        },
        Template {
            name: "square-minimal".into(),
            description: "Square 1:1 for feeds, minimal captions".into(),
            captions: Some(CaptionStyle::minimal()),
            reframe: Some(ReframeSpec::square()),
            silence: None,
            remove_fillers: false,
            broll: false,
            music: None,
        },
        Template {
            name: "landscape-captions".into(),
            description: "Keep 16:9, classic captions, remove filler words".into(),
            captions: Some(CaptionStyle::classic()),
            reframe: None,
            silence: None,
            remove_fillers: true,
            broll: false,
            music: None,
        },
    ]
}

pub fn builtin(name: &str) -> Option<Template> {
    builtin_templates().into_iter().find(|t| t.name == name)
}

/// Built-ins plus any `*.json` templates in `dir` (user templates win on name clash).
pub async fn load_templates(dir: Option<&Path>) -> Result<Vec<Template>> {
    let mut all = builtin_templates();
    if let Some(dir) = dir {
        if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
            while let Some(e) = rd.next_entry().await? {
                if e.path().extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(t) = serde_json::from_slice::<Template>(&tokio::fs::read(e.path()).await?) {
                    all.retain(|x| x.name != t.name);
                    all.push(t);
                }
            }
        }
    }
    Ok(all)
}

/// Apply to a clip. Does not touch the cut list the user edited by hand.
pub fn apply_template(clip: &mut Clip, t: &Template) {
    clip.captions.clone_from(&t.captions);
    clip.reframe.clone_from(&t.reframe);
    clip.silence = t.silence;
    clip.remove_fillers = t.remove_fillers;
    if t.music.is_some() {
        clip.music.clone_from(&t.music);
    }
    clip.template = Some(t.name.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_unique_and_applicable() {
        let all = builtin_templates();
        let mut names: Vec<_> = all.iter().map(|t| t.name.clone()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), all.len());
        let mut c = Clip::new("c", "t", 0.0, 10.0);
        c.deleted_words.insert(3);
        apply_template(&mut c, &builtin("shorts-karaoke").unwrap());
        assert_eq!(c.template.as_deref(), Some("shorts-karaoke"));
        assert!(c.reframe.is_some() && c.remove_fillers && c.silence.is_some());
        assert!(c.deleted_words.contains(&3), "manual edits survive");
    }

    #[tokio::test]
    async fn user_template_overrides() {
        let d = std::env::temp_dir().join(format!("hk_tpl_{}", crate::project::new_id("t")));
        tokio::fs::create_dir_all(&d).await.unwrap();
        let mut t = builtin("clean-vertical").unwrap();
        t.description = "mine".into();
        tokio::fs::write(d.join("a.json"), serde_json::to_vec(&t).unwrap()).await.unwrap();
        let all = load_templates(Some(&d)).await.unwrap();
        assert_eq!(all.iter().filter(|x| x.name == "clean-vertical").count(), 1);
        assert_eq!(all.iter().find(|x| x.name == "clean-vertical").unwrap().description, "mine");
        tokio::fs::remove_dir_all(&d).await.unwrap();
    }
}
