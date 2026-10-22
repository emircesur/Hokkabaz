//! Titles, descriptions and hashtags per platform, within each platform's limits.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::llm::{Llm, complete_json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    YoutubeShorts,
    Tiktok,
    InstagramReels,
    X,
    Linkedin,
}

struct Limits {
    title: usize,
    description: usize,
    hashtags: usize,
}

impl Platform {
    pub const ALL: [Platform; 5] = [
        Platform::YoutubeShorts,
        Platform::Tiktok,
        Platform::InstagramReels,
        Platform::X,
        Platform::Linkedin,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::YoutubeShorts => "youtube_shorts",
            Self::Tiktok => "tiktok",
            Self::InstagramReels => "instagram_reels",
            Self::X => "x",
            Self::Linkedin => "linkedin",
        }
    }

    fn limits(self) -> Limits {
        match self {
            Self::YoutubeShorts => Limits {
                title: 100,
                description: 5000,
                hashtags: 5,
            },
            Self::Tiktok => Limits {
                title: 90,
                description: 2200,
                hashtags: 5,
            },
            Self::InstagramReels => Limits {
                title: 125,
                description: 2200,
                hashtags: 10,
            },
            Self::X => Limits {
                title: 0,
                description: 270,
                hashtags: 2,
            },
            Self::Linkedin => Limits {
                title: 150,
                description: 3000,
                hashtags: 4,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlatformPost {
    pub title: String,
    pub description: String,
    pub hashtags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SocialPack {
    pub posts: BTreeMap<String, PlatformPost>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SocialOpts {
    pub tone: Option<String>,
    /// Output language name ("English", "Türkçe"); default: language of the transcript.
    pub language: Option<String>,
    pub call_to_action: Option<String>,
}

const SYSTEM: &str = "You write social media copy for short vertical videos. For each requested platform write a scroll-stopping \
title (a hook, not clickbait lies), a description with one clear call to action, and relevant hashtags (no spaces, may omit '#'). \
Stay truthful to the transcript. Reply with ONLY JSON: {\"posts\":{\"<platform>\":{\"title\":string,\"description\":string,\"hashtags\":[string]}}}.";

#[derive(Deserialize)]
struct Out {
    #[serde(default)]
    posts: BTreeMap<String, PlatformPost>,
}

/// Cut `s` to at most `max` chars on a word boundary, adding an ellipsis when cut.
pub fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return s.chars().take(max).collect();
    }
    let cut: String = s.chars().take(max - 1).collect();
    let cut = cut
        .rfind(char::is_whitespace)
        .map_or(cut.as_str(), |i| &cut[..i])
        .trim_end_matches([',', ';', ':', '-', ' ']);
    format!("{cut}…")
}

/// `#Tag`, no spaces / punctuation, deduped case-insensitively.
pub fn normalize_hashtags(tags: &[String], max: usize) -> Vec<String> {
    let mut seen = Vec::<String>::new();
    let mut out = Vec::new();
    for t in tags {
        let core: String = t
            .trim()
            .trim_start_matches('#')
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if core.is_empty() || seen.contains(&core.to_lowercase()) {
            continue;
        }
        seen.push(core.to_lowercase());
        out.push(format!("#{core}"));
        if out.len() >= max {
            break;
        }
    }
    out
}

fn clamp_post(p: Platform, mut post: PlatformPost) -> PlatformPost {
    let l = p.limits();
    post.title = truncate(&post.title, l.title);
    post.hashtags = normalize_hashtags(&post.hashtags, l.hashtags);
    // The description must leave room for hashtags on platforms that inline them (X).
    let tags = post.hashtags.join(" ");
    let room = if p == Platform::X {
        l.description.saturating_sub(tags.chars().count() + 1)
    } else {
        l.description
    };
    post.description = truncate(&post.description, room);
    post
}

/// Generate posts for `platforms` from the clip's transcript text.
pub async fn write_social(
    llm: &dyn Llm,
    clip_title: &str,
    text: &str,
    platforms: &[Platform],
    opts: &SocialOpts,
) -> Result<SocialPack> {
    let names: Vec<&str> = platforms.iter().map(|p| p.key()).collect();
    let mut user = format!(
        "Platforms: {}.\nClip working title: {clip_title}\n",
        names.join(", ")
    );
    if let Some(t) = &opts.tone {
        user.push_str(&format!("Tone: {t}\n"));
    }
    if let Some(l) = &opts.language {
        user.push_str(&format!("Write in: {l}\n"));
    }
    if let Some(c) = &opts.call_to_action {
        user.push_str(&format!("Call to action: {c}\n"));
    }
    let limits: Vec<String> = platforms
        .iter()
        .map(|p| {
            format!(
                "{} (title<={}, description<={}, hashtags<={})",
                p.key(),
                p.limits().title,
                p.limits().description,
                p.limits().hashtags
            )
        })
        .collect();
    user.push_str(&format!(
        "Limits: {}\n\nTranscript:\n{text}",
        limits.join("; ")
    ));
    let out: Out = complete_json(llm, SYSTEM, &user).await?;
    let mut pack = SocialPack::default();
    for p in platforms {
        if let Some(post) = out.posts.get(p.key()) {
            pack.posts
                .insert(p.key().to_string(), clamp_post(*p, post.clone()));
        }
    }
    Ok(pack)
}

impl SocialPack {
    /// Copy-paste text for one platform.
    pub fn render(&self, platform: Platform) -> Option<String> {
        let p = self.posts.get(platform.key())?;
        let mut s = String::new();
        if !p.title.is_empty() {
            s.push_str(&p.title);
            s.push_str("\n\n");
        }
        s.push_str(&p.description);
        if !p.hashtags.is_empty() {
            s.push_str("\n\n");
            s.push_str(&p.hashtags.join(" "));
        }
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ScriptedLlm;

    #[test]
    fn truncates_on_word_boundary() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("hello wonderful world", 14), "hello…");
        assert!(truncate("a".repeat(50).as_str(), 10).chars().count() <= 10);
    }

    #[test]
    fn hashtags_normalized() {
        let t = vec![
            "#Fun".into(),
            "fun".into(),
            "very cool!".into(),
            "".into(),
            "#a_b".into(),
            "x".into(),
        ];
        assert_eq!(normalize_hashtags(&t, 3), vec!["#Fun", "#verycool", "#a_b"]);
    }

    #[tokio::test]
    async fn posts_are_clamped_per_platform() {
        let long = "word ".repeat(100);
        let reply = format!(
            r#"{{"posts":{{"x":{{"title":"ignored","description":"{long}","hashtags":["a","b","c"]}},"tiktok":{{"title":"T","description":"d","hashtags":["x"]}}}}}}"#
        );
        let llm = ScriptedLlm::new([reply]);
        let pack = write_social(
            &llm,
            "t",
            "text",
            &[Platform::X, Platform::Tiktok, Platform::Linkedin],
            &SocialOpts::default(),
        )
        .await
        .unwrap();
        assert_eq!(pack.posts.len(), 2, "missing platform omitted");
        let x = &pack.posts["x"];
        assert_eq!(x.title, "");
        assert_eq!(x.hashtags.len(), 2);
        assert!(x.description.chars().count() + 1 + x.hashtags.join(" ").chars().count() <= 270);
        assert!(pack.render(Platform::Tiktok).unwrap().contains("#x"));
        assert!(pack.render(Platform::Linkedin).is_none());
        assert!(llm.prompts.lock().unwrap()[0].1.contains("Limits:"));
    }
}
