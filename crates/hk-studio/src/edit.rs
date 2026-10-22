//! Edit operations shared by the UI and the agent, with undo/redo.
//!
//! Everything that changes a clip goes through [`EditOp`], so a click in the
//! transcript editor and an agent tool call are the same thing.

use hk_stt::Transcript;
use serde::{Deserialize, Serialize};

use crate::broll::BrollPlacement;
use crate::captions::CaptionStyle;
use crate::error::{Result, StudioError};
use crate::project::{Clip, MusicSpec, SilenceCut, StudioProject, new_id};
use crate::reframe::ReframeSpec;
use crate::templates::{apply_template, builtin};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    AddClip {
        title: String,
        start: f64,
        end: f64,
    },
    RemoveClip {
        clip: String,
    },
    RenameClip {
        clip: String,
        title: String,
    },
    SetBounds {
        clip: String,
        start: f64,
        end: f64,
    },
    SplitClip {
        clip: String,
        at: f64,
    },
    DeleteWords {
        clip: String,
        words: Vec<usize>,
    },
    RestoreWords {
        clip: String,
        words: Vec<usize>,
    },
    /// Delete every word that lies fully inside `start..end` (source seconds).
    DeleteRange {
        clip: String,
        start: f64,
        end: f64,
    },
    SetFillers {
        clip: String,
        on: bool,
    },
    SetSilence {
        clip: String,
        silence: Option<SilenceCut>,
    },
    SetCaptions {
        clip: String,
        style: Option<CaptionStyle>,
    },
    SetCaptionPreset {
        clip: String,
        preset: String,
    },
    SetReframe {
        clip: String,
        reframe: Option<ReframeSpec>,
    },
    SetBroll {
        clip: String,
        broll: Vec<BrollPlacement>,
    },
    RemoveBroll {
        clip: String,
        index: usize,
    },
    SetMusic {
        clip: String,
        music: Option<MusicSpec>,
    },
    ApplyTemplate {
        clip: String,
        template: String,
    },
}

impl EditOp {
    /// Clip this op touches, if one.
    pub fn clip(&self) -> Option<&str> {
        match self {
            Self::AddClip { .. } => None,
            Self::RemoveClip { clip }
            | Self::RenameClip { clip, .. }
            | Self::SetBounds { clip, .. }
            | Self::SplitClip { clip, .. }
            | Self::DeleteWords { clip, .. }
            | Self::RestoreWords { clip, .. }
            | Self::DeleteRange { clip, .. }
            | Self::SetFillers { clip, .. }
            | Self::SetSilence { clip, .. }
            | Self::SetCaptions { clip, .. }
            | Self::SetCaptionPreset { clip, .. }
            | Self::SetReframe { clip, .. }
            | Self::SetBroll { clip, .. }
            | Self::RemoveBroll { clip, .. }
            | Self::SetMusic { clip, .. }
            | Self::ApplyTemplate { clip, .. } => Some(clip),
        }
    }
}

/// Words fully inside `start..end`.
pub fn words_in(tr: &Transcript, start: f64, end: f64) -> Vec<usize> {
    tr.words
        .iter()
        .enumerate()
        .filter(|(_, w)| w.start >= start - 1e-6 && w.end <= end + 1e-6)
        .map(|(i, _)| i)
        .collect()
}

/// Apply `op`. Returns the id of the clip it created or changed, if any.
pub fn apply(p: &mut StudioProject, op: &EditOp) -> Result<Option<String>> {
    match op {
        EditOp::AddClip { title, start, end } => {
            let c = p.add_clip(title.clone(), *start, *end)?;
            Ok(Some(c.id.clone()))
        }
        EditOp::RemoveClip { clip } => {
            let before = p.clips.len();
            p.clips.retain(|c| &c.id != clip);
            if p.clips.len() == before {
                return Err(StudioError::NotFound(format!("clip {clip}")));
            }
            Ok(None)
        }
        EditOp::RenameClip { clip, title } => {
            p.clip_mut(clip)?.title.clone_from(title);
            Ok(Some(clip.clone()))
        }
        EditOp::SetBounds { clip, start, end } => {
            let dur = p.source.duration;
            let (s, e) = (start.max(0.0), end.min(dur));
            if (e - s).is_nan() || e - s <= 0.1 {
                return Err(StudioError::Invalid(format!("empty range {start}..{end}")));
            }
            let c = p.clip_mut(clip)?;
            c.start = s;
            c.end = e;
            Ok(Some(clip.clone()))
        }
        EditOp::SplitClip { clip, at } => {
            let idx = p
                .clips
                .iter()
                .position(|c| &c.id == clip)
                .ok_or_else(|| StudioError::NotFound(format!("clip {clip}")))?;
            let orig = p.clips[idx].clone();
            if !(*at > orig.start + 0.5 && *at < orig.end - 0.5) {
                return Err(StudioError::Invalid(
                    "split point must be inside the clip, 0.5s from each end".into(),
                ));
            }
            let mut second = orig.clone();
            second.id = new_id("c");
            second.title = format!("{} (2)", orig.title);
            second.start = *at;
            second.score = None;
            second.social = None;
            p.clips[idx].end = *at;
            p.clips[idx].score = None;
            p.clips[idx].social = None;
            p.clips.insert(idx + 1, second.clone());
            Ok(Some(second.id))
        }
        EditOp::DeleteWords { clip, words } => {
            p.set_words_deleted(clip, words.iter().copied(), true)?;
            Ok(Some(clip.clone()))
        }
        EditOp::RestoreWords { clip, words } => {
            p.set_words_deleted(clip, words.iter().copied(), false)?;
            Ok(Some(clip.clone()))
        }
        EditOp::DeleteRange { clip, start, end } => {
            let words = words_in(p.transcript()?, *start, *end);
            p.set_words_deleted(clip, words, true)?;
            Ok(Some(clip.clone()))
        }
        EditOp::SetFillers { clip, on } => {
            p.clip_mut(clip)?.remove_fillers = *on;
            Ok(Some(clip.clone()))
        }
        EditOp::SetSilence { clip, silence } => {
            p.clip_mut(clip)?.silence = *silence;
            Ok(Some(clip.clone()))
        }
        EditOp::SetCaptions { clip, style } => {
            p.clip_mut(clip)?.captions.clone_from(style);
            Ok(Some(clip.clone()))
        }
        EditOp::SetCaptionPreset { clip, preset } => {
            let st = CaptionStyle::preset(preset)
                .ok_or_else(|| StudioError::Invalid(format!("unknown caption preset {preset}")))?;
            p.clip_mut(clip)?.captions = Some(st);
            Ok(Some(clip.clone()))
        }
        EditOp::SetReframe { clip, reframe } => {
            p.clip_mut(clip)?.reframe.clone_from(reframe);
            Ok(Some(clip.clone()))
        }
        EditOp::SetBroll { clip, broll } => {
            p.clip_mut(clip)?.broll.clone_from(broll);
            Ok(Some(clip.clone()))
        }
        EditOp::RemoveBroll { clip, index } => {
            let c = p.clip_mut(clip)?;
            if *index >= c.broll.len() {
                return Err(StudioError::Invalid(format!("no b-roll #{index}")));
            }
            c.broll.remove(*index);
            Ok(Some(clip.clone()))
        }
        EditOp::SetMusic { clip, music } => {
            p.clip_mut(clip)?.music.clone_from(music);
            Ok(Some(clip.clone()))
        }
        EditOp::ApplyTemplate { clip, template } => {
            let t = builtin(template)
                .ok_or_else(|| StudioError::Invalid(format!("unknown template {template}")))?;
            apply_template(p.clip_mut(clip)?, &t);
            Ok(Some(clip.clone()))
        }
    }
}

/// Clip list history for undo / redo (cheap: clips are small).
#[derive(Debug, Default, Clone)]
pub struct History {
    undo: Vec<Vec<Clip>>,
    redo: Vec<Vec<Clip>>,
}

const HISTORY_CAP: usize = 200;

impl History {
    /// Remember the state before a change.
    pub fn record(&mut self, p: &StudioProject) {
        self.undo.push(p.clips.clone());
        if self.undo.len() > HISTORY_CAP {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, p: &mut StudioProject) -> bool {
        match self.undo.pop() {
            Some(prev) => {
                self.redo.push(std::mem::replace(&mut p.clips, prev));
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self, p: &mut StudioProject) -> bool {
        match self.redo.pop() {
            Some(next) => {
                self.undo.push(std::mem::replace(&mut p.clips, next));
                true
            }
            None => false,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hk_stt::Word;

    use super::*;
    use crate::project::SourceRef;

    fn project() -> StudioProject {
        let mut p = StudioProject::new(
            "t",
            SourceRef {
                path: PathBuf::from("a.mp4"),
                duration: 100.0,
                width: 1920,
                height: 1080,
                fps: 30.0,
                has_audio: true,
            },
        );
        p.transcript = Some(Transcript {
            language: None,
            duration: 100.0,
            words: (0..10)
                .map(|i| Word::new(format!("w{i}"), i as f64, i as f64 + 0.8))
                .collect(),
            segments: vec![],
        });
        p
    }

    #[test]
    fn word_edits_and_range_delete() {
        let mut p = project();
        let id = apply(
            &mut p,
            &EditOp::AddClip {
                title: "a".into(),
                start: 0.0,
                end: 10.0,
            },
        )
        .unwrap()
        .unwrap();
        apply(
            &mut p,
            &EditOp::DeleteWords {
                clip: id.clone(),
                words: vec![1, 2],
            },
        )
        .unwrap();
        apply(
            &mut p,
            &EditOp::DeleteRange {
                clip: id.clone(),
                start: 5.0,
                end: 7.9,
            },
        )
        .unwrap();
        let c = p.clip(&id).unwrap();
        assert_eq!(
            c.deleted_words.iter().copied().collect::<Vec<_>>(),
            vec![1, 2, 5, 6, 7]
        );
        apply(
            &mut p,
            &EditOp::RestoreWords {
                clip: id.clone(),
                words: vec![1],
            },
        )
        .unwrap();
        assert!(!p.clip(&id).unwrap().deleted_words.contains(&1));
        assert!(
            apply(
                &mut p,
                &EditOp::DeleteWords {
                    clip: id,
                    words: vec![99]
                }
            )
            .is_err()
        );
    }

    #[test]
    fn split_bounds_and_remove() {
        let mut p = project();
        let id = apply(
            &mut p,
            &EditOp::AddClip {
                title: "a".into(),
                start: 10.0,
                end: 50.0,
            },
        )
        .unwrap()
        .unwrap();
        let second = apply(
            &mut p,
            &EditOp::SplitClip {
                clip: id.clone(),
                at: 30.0,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(p.clips.len(), 2);
        assert_eq!(
            (p.clip(&id).unwrap().end, p.clip(&second).unwrap().start),
            (30.0, 30.0)
        );
        assert!(
            apply(
                &mut p,
                &EditOp::SplitClip {
                    clip: id.clone(),
                    at: 10.1
                }
            )
            .is_err()
        );
        apply(
            &mut p,
            &EditOp::SetBounds {
                clip: id.clone(),
                start: -5.0,
                end: 500.0,
            },
        )
        .unwrap();
        assert_eq!(
            (p.clip(&id).unwrap().start, p.clip(&id).unwrap().end),
            (0.0, 100.0)
        );
        apply(
            &mut p,
            &EditOp::RemoveClip {
                clip: second.clone(),
            },
        )
        .unwrap();
        assert!(apply(&mut p, &EditOp::RemoveClip { clip: second }).is_err());
    }

    #[test]
    fn presets_templates_and_errors() {
        let mut p = project();
        let id = apply(
            &mut p,
            &EditOp::AddClip {
                title: "a".into(),
                start: 0.0,
                end: 10.0,
            },
        )
        .unwrap()
        .unwrap();
        apply(
            &mut p,
            &EditOp::SetCaptionPreset {
                clip: id.clone(),
                preset: "pop".into(),
            },
        )
        .unwrap();
        assert!(p.clip(&id).unwrap().captions.is_some());
        assert!(
            apply(
                &mut p,
                &EditOp::SetCaptionPreset {
                    clip: id.clone(),
                    preset: "nope".into()
                }
            )
            .is_err()
        );
        apply(
            &mut p,
            &EditOp::ApplyTemplate {
                clip: id.clone(),
                template: "shorts-karaoke".into(),
            },
        )
        .unwrap();
        assert!(p.clip(&id).unwrap().reframe.is_some());
        assert!(
            apply(
                &mut p,
                &EditOp::ApplyTemplate {
                    clip: id,
                    template: "zzz".into()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn undo_redo() {
        let mut p = project();
        let mut h = History::default();
        h.record(&p);
        let id = apply(
            &mut p,
            &EditOp::AddClip {
                title: "a".into(),
                start: 0.0,
                end: 10.0,
            },
        )
        .unwrap()
        .unwrap();
        h.record(&p);
        apply(
            &mut p,
            &EditOp::DeleteWords {
                clip: id.clone(),
                words: vec![3],
            },
        )
        .unwrap();
        assert!(h.undo(&mut p));
        assert!(p.clip(&id).unwrap().deleted_words.is_empty());
        assert!(h.undo(&mut p));
        assert!(p.clips.is_empty());
        assert!(!h.undo(&mut p));
        assert!(h.redo(&mut p) && h.redo(&mut p));
        assert!(p.clip(&id).unwrap().deleted_words.contains(&3));
        // A new edit clears redo.
        assert!(h.undo(&mut p));
        h.record(&p);
        assert!(!h.can_redo());
    }

    #[test]
    fn ops_roundtrip_json() {
        let op = EditOp::DeleteWords {
            clip: "c1".into(),
            words: vec![1, 2],
        };
        let s = serde_json::to_string(&op).unwrap();
        assert!(s.contains("\"op\":\"delete_words\""));
        assert_eq!(serde_json::from_str::<EditOp>(&s).unwrap(), op);
    }
}
