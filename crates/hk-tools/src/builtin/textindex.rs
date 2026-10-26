//! Small BM25 ranker shared by the memory and knowledge tools (no external index).

use std::collections::HashMap;

/// Lowercased alphanumeric tokens (Unicode-aware), length ≥ 2.
pub fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 2)
        .map(str::to_lowercase)
        .collect()
}

/// Rank `docs` for `query`; returns (doc index, score) best first, score > 0 only.
pub fn bm25(query: &str, docs: &[&str], k: usize) -> Vec<(usize, f64)> {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    let q: Vec<String> = tokenize(query);
    if q.is_empty() || docs.is_empty() {
        return Vec::new();
    }
    let toks: Vec<Vec<String>> = docs.iter().map(|d| tokenize(d)).collect();
    let n = toks.len() as f64;
    let avg = toks.iter().map(Vec::len).sum::<usize>() as f64 / n;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for t in &toks {
        let mut seen: Vec<&str> = t.iter().map(String::as_str).collect();
        seen.sort_unstable();
        seen.dedup();
        for w in seen {
            *df.entry(w).or_default() += 1;
        }
    }
    let mut scored: Vec<(usize, f64)> = toks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let len = t.len() as f64;
            let score = q
                .iter()
                .map(|w| {
                    let f = t.iter().filter(|x| *x == w).count() as f64;
                    if f == 0.0 {
                        return 0.0;
                    }
                    let d = *df.get(w.as_str()).unwrap_or(&0) as f64;
                    let idf = ((n - d + 0.5) / (d + 0.5) + 1.0).ln();
                    idf * f * (K1 + 1.0) / (f + K1 * (1.0 - B + B * len / avg.max(1.0)))
                })
                .sum::<f64>();
            (i, score)
        })
        .filter(|(_, s)| *s > 0.0)
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(k);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_relevant_first() {
        let docs = [
            "the cat sat",
            "rust async runtime tokio",
            "tokio tokio scheduler",
        ];
        let r = bm25("tokio scheduler", &docs, 5);
        assert_eq!(r.iter().map(|x| x.0).collect::<Vec<_>>(), vec![2, 1]);
        assert!(bm25("", &docs, 5).is_empty());
        assert_eq!(
            tokenize("Şey, İstanbul-2026!"),
            vec!["şey", "i̇stanbul", "2026"]
        );
    }
}
