//! Per-language filler word detection.

use std::collections::BTreeSet;

use crate::transcript::{Transcript, normalize_token};

/// Filler words and phrases for one language. Lists are deliberately minimal;
/// extend with [`FillerSet::add`] or replace via config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FillerSet {
    words: BTreeSet<String>,
    /// Multi-word fillers, each already normalised per token.
    phrases: Vec<Vec<String>>,
}

/// (hesitations, discourse fillers that are sometimes real words) per language.
fn lists(lang: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    Some(match lang {
        "en" => (
            &["um", "uh", "uhm", "umm", "uhh", "er", "erm", "ah", "hmm", "mm", "mhm", "you know", "i mean"],
            &["like", "basically", "literally", "actually", "sort of", "kind of"],
        ),
        "tr" => (
            &["ee", "eee", "eeee", "ıı", "ııı", "hımm", "hmm", "yani", "şey", "işte"],
            &["ya", "falan", "filan", "böyle", "hani"],
        ),
        "de" => (
            &["äh", "ähm", "äm", "öh", "öhm", "ehm", "hm", "hmm", "mhm"],
            &["also", "halt", "sozusagen", "quasi", "ja", "na ja"],
        ),
        "es" => (
            &["eh", "ehh", "em", "emm", "mm", "mmm", "hmm", "o sea"],
            &["este", "pues", "bueno", "digamos", "vale"],
        ),
        "fr" => (
            &["euh", "heu", "euhh", "hum", "hmm", "bah", "mmh"],
            &["ben", "genre", "quoi", "voilà", "en fait", "tu vois", "tu sais"],
        ),
        _ => return None,
    })
}

/// Map a language tag or English language name to a 2-letter code.
pub fn language_code(lang: &str) -> String {
    let l = lang.trim().to_lowercase();
    let code = match l.as_str() {
        "english" => "en",
        "turkish" => "tr",
        "german" => "de",
        "spanish" => "es",
        "french" => "fr",
        _ => l.split(['-', '_']).next().unwrap_or(""),
    };
    code.to_string()
}

impl FillerSet {
    /// Empty set (nothing flagged).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Default list for `lang` (code like `en`, `tr-TR`, or English name).
    /// `aggressive` adds discourse fillers that are also normal words ("like",
    /// "yani", "pues"). Unknown language gives an empty set.
    pub fn for_language(lang: &str, aggressive: bool) -> Self {
        let mut set = Self::empty();
        if let Some((strict, loose)) = lists(&language_code(lang)) {
            for w in strict {
                set.add(w);
            }
            if aggressive {
                for w in loose {
                    set.add(w);
                }
            }
        }
        set
    }

    /// Add a word or space-separated phrase.
    pub fn add(&mut self, entry: &str) {
        let toks: Vec<String> = entry.split_whitespace().map(normalize_token).filter(|t| !t.is_empty()).collect();
        match toks.len() {
            0 => {}
            1 => {
                self.words.insert(toks.into_iter().next().unwrap_or_default());
            }
            _ => {
                if !self.phrases.contains(&toks) {
                    self.phrases.push(toks);
                }
            }
        }
    }

    /// Remove a word or phrase (e.g. drop "yani" for a speaker who means it).
    pub fn remove(&mut self, entry: &str) {
        let toks: Vec<String> = entry.split_whitespace().map(normalize_token).collect();
        if toks.len() == 1 {
            self.words.remove(&toks[0]);
        } else {
            self.phrases.retain(|p| *p != toks);
        }
    }

    /// Build from an explicit list (config override).
    pub fn from_entries<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut set = Self::empty();
        for e in entries {
            set.add(e.as_ref());
        }
        set
    }

    /// True when nothing would be flagged.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && self.phrases.is_empty()
    }

    /// Single token match.
    pub fn contains(&self, token: &str) -> bool {
        self.words.contains(&normalize_token(token))
    }

    /// Set `flags.filler` on matching words (clears it elsewhere). Returns the
    /// number flagged. Language falls back to `transcript.language`.
    pub fn mark(&self, t: &mut Transcript) -> usize {
        let norm: Vec<String> = t.words.iter().map(|w| normalize_token(&w.text)).collect();
        let mut flags = vec![false; norm.len()];
        for (i, n) in norm.iter().enumerate() {
            if self.words.contains(n) {
                flags[i] = true;
            }
        }
        for p in &self.phrases {
            let mut i = 0;
            while i + p.len() <= norm.len() {
                if norm[i..i + p.len()] == p[..] {
                    flags[i..i + p.len()].iter_mut().for_each(|f| *f = true);
                    i += p.len();
                } else {
                    i += 1;
                }
            }
        }
        let mut count = 0;
        for (w, f) in t.words.iter_mut().zip(flags) {
            w.flags.filler = f;
            count += usize::from(f);
        }
        count
    }

    /// Default set for the transcript's own language.
    pub fn for_transcript(t: &Transcript, aggressive: bool) -> Self {
        Self::for_language(t.language.as_deref().unwrap_or("en"), aggressive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::Word;

    fn t(lang: &str, words: &[&str]) -> Transcript {
        Transcript {
            language: Some(lang.into()),
            duration: 10.0,
            words: words.iter().enumerate().map(|(i, w)| Word::new(*w, i as f64, i as f64 + 0.5)).collect(),
            segments: vec![],
        }
    }

    #[test]
    fn english_strict_and_aggressive() {
        let mut tr = t("english", &["So,", "um,", "I", "mean", "it's", "like", "Uh...", "good"]);
        let strict = FillerSet::for_transcript(&tr, false);
        assert_eq!(strict.mark(&mut tr), 4); // um, "I mean" (2 words), Uh
        assert_eq!(tr.filler_indices().into_iter().collect::<Vec<_>>(), vec![1, 2, 3, 6]);
    }

    #[test]
    fn aggressive_adds_like() {
        let mut tr = t("en", &["it's", "like", "good"]);
        assert_eq!(FillerSet::for_language("en", false).mark(&mut tr), 0);
        assert_eq!(FillerSet::for_language("en-US", true).mark(&mut tr), 1);
        assert!(tr.words[1].flags.filler);
    }

    #[test]
    fn per_language_and_unknown() {
        let mut tr = t("tr", &["şey", "bugün", "ııı", "hava", "Yani"]);
        assert_eq!(FillerSet::for_language("tr", false).mark(&mut tr), 3);
        let mut de = t("de", &["Äh,", "ja", "ähm"]);
        assert_eq!(FillerSet::for_language("de", false).mark(&mut de), 2);
        let mut es = t("es", &["eh", "o", "sea", "bien"]);
        assert_eq!(FillerSet::for_language("es", false).mark(&mut es), 3);
        let mut fr = t("fr", &["euh", "oui"]);
        assert_eq!(FillerSet::for_language("fr", false).mark(&mut fr), 1);
        assert!(FillerSet::for_language("xx", true).is_empty());
    }

    #[test]
    fn configurable_and_remarks_clear_old_flags() {
        let mut tr = t("en", &["um", "banana"]);
        let mut set = FillerSet::for_language("en", false);
        set.add("banana");
        set.remove("um");
        assert_eq!(set.mark(&mut tr), 1);
        assert!(!tr.words[0].flags.filler && tr.words[1].flags.filler);
        assert_eq!(FillerSet::from_entries(["xx yy"]).mark(&mut tr), 0);
    }
}
