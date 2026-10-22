//! B-roll: pick moments and queries with the LLM, fetch stock footage, place it.
//!
//! Providers: Pexels and Pixabay (user API key) and a local folder. Base URLs
//! are overridable so tests can point at a loopback server.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::captions::CapWord;
use crate::error::{Result, StudioError};
use crate::llm::{Llm, complete_json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Image,
    Video,
}

/// B-roll placed on the OUTPUT timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrollPlacement {
    pub start: f64,
    pub end: f64,
    pub query: String,
    pub asset: PathBuf,
    pub kind: AssetKind,
    #[serde(default)]
    pub credit: Option<String>,
}

/// Where and what to look for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrollRequest {
    pub start: f64,
    pub end: f64,
    pub query: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrollOpts {
    /// Max inserts.
    pub max: usize,
    pub min_len: f64,
    pub max_len: f64,
    /// Minimum spacing between inserts and from clip start/end.
    pub min_gap: f64,
    pub lead_in: f64,
}

impl Default for BrollOpts {
    fn default() -> Self {
        Self {
            max: 4,
            min_len: 1.5,
            max_len: 4.0,
            min_gap: 4.0,
            lead_in: 2.5,
        }
    }
}

const SYSTEM: &str = "You plan B-roll for a short vertical video. From the timestamped transcript choose moments \
where a stock clip would illustrate what is said (concrete nouns, places, actions - not abstract ideas). \
Give a short English stock-footage search query (2-4 words) for each. \
Reply with ONLY JSON: {\"moments\":[{\"start\":number,\"end\":number,\"query\":string}]}; times are seconds on the clip timeline.";

#[derive(Deserialize)]
struct Moments {
    #[serde(default)]
    moments: Vec<BrollRequest>,
}

/// Ask the LLM where to cut away. `words` are on the output timeline.
pub async fn plan_broll(
    llm: &dyn Llm,
    words: &[CapWord],
    opts: &BrollOpts,
) -> Result<Vec<BrollRequest>> {
    let end = words.last().map_or(0.0, |w| w.end);
    if words.is_empty() || end < opts.lead_in + opts.min_len {
        return Ok(Vec::new());
    }
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut t0 = words[0].start;
    for w in words {
        if !cur.is_empty() && (w.start - t0 > 4.0 || cur.ends_with(['.', '!', '?'])) {
            lines.push(format!("[{t0:.1}] {}", cur.trim()));
            cur.clear();
            t0 = w.start;
        }
        cur.push_str(&w.text);
        cur.push(' ');
    }
    if !cur.trim().is_empty() {
        lines.push(format!("[{t0:.1}] {}", cur.trim()));
    }
    let user = format!(
        "Clip length {end:.1}s. Up to {} moments of {:.1}-{:.1}s, none before {:.1}s, at least {:.1}s apart.\n\n{}",
        opts.max,
        opts.min_len,
        opts.max_len,
        opts.lead_in,
        opts.min_gap,
        lines.join("\n")
    );
    let out: Moments = complete_json(llm, SYSTEM, &user).await?;
    Ok(sanitize(out.moments, end, opts))
}

/// Clamp, order and space the model's moments.
pub fn sanitize(mut m: Vec<BrollRequest>, clip_end: f64, opts: &BrollOpts) -> Vec<BrollRequest> {
    m.retain(|r| r.start.is_finite() && r.end.is_finite() && !r.query.trim().is_empty());
    m.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<BrollRequest> = Vec::new();
    for mut r in m {
        r.start = r.start.max(opts.lead_in);
        r.end = r.end.min(r.start + opts.max_len).min(clip_end - 0.5);
        if r.end - r.start < opts.min_len {
            continue;
        }
        if out.last().is_some_and(|p| r.start - p.end < opts.min_gap) {
            continue;
        }
        r.query = r.query.trim().to_string();
        out.push(r);
        if out.len() >= opts.max {
            break;
        }
    }
    out
}

/// One search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct StockAsset {
    pub id: String,
    pub url: String,
    pub kind: AssetKind,
    pub width: u32,
    pub height: u32,
    pub credit: String,
}

#[async_trait]
pub trait StockProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn search(&self, query: &str, kind: AssetKind, limit: usize) -> Result<Vec<StockAsset>>;
    /// Make the asset available as a local file under `dir`.
    async fn fetch(&self, asset: &StockAsset, dir: &Path) -> Result<PathBuf>;
}

const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;

async fn download(
    client: &reqwest::Client,
    url: &str,
    dir: &Path,
    stem: &str,
    ext: &str,
) -> Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await?;
    let dest = dir.join(format!("{stem}.{ext}"));
    if tokio::fs::metadata(&dest).await.is_ok_and(|m| m.len() > 0) {
        return Ok(dest);
    }
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| StudioError::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(StudioError::Http(format!(
            "download {}: {}",
            resp.status(),
            url
        )));
    }
    if resp.content_length().is_some_and(|n| n > MAX_DOWNLOAD) {
        return Err(StudioError::Http("asset too large".into()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| StudioError::Http(e.to_string()))?;
    if bytes.len() as u64 > MAX_DOWNLOAD {
        return Err(StudioError::Http("asset too large".into()));
    }
    let tmp = dest.with_extension(format!("{ext}.part"));
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::rename(&tmp, &dest).await?;
    Ok(dest)
}

fn safe_stem(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(60)
        .collect()
}

fn ext_of(url: &str, default: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.rsplit('.')
        .next()
        .filter(|e| e.len() <= 4 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or(default)
        .to_lowercase()
}

/// Pexels (https://www.pexels.com/api/).
#[derive(Debug, Clone)]
pub struct Pexels {
    key: String,
    base: String,
    client: reqwest::Client,
}

impl Pexels {
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            base: "https://api.pexels.com".into(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }
}

#[async_trait]
impl StockProvider for Pexels {
    fn name(&self) -> &str {
        "pexels"
    }

    async fn search(&self, query: &str, kind: AssetKind, limit: usize) -> Result<Vec<StockAsset>> {
        let (path, orient) = match kind {
            AssetKind::Video => ("videos/search", "portrait"),
            AssetKind::Image => ("v1/search", "portrait"),
        };
        let resp = self
            .client
            .get(format!("{}/{path}", self.base))
            .header("Authorization", &self.key)
            .query(&[
                ("query", query),
                ("per_page", &limit.to_string()),
                ("orientation", orient),
            ])
            .send()
            .await
            .map_err(|e| StudioError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(StudioError::Http(format!("pexels {}", resp.status())));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| StudioError::Http(e.to_string()))?;
        Ok(parse_pexels(&v, kind))
    }

    async fn fetch(&self, a: &StockAsset, dir: &Path) -> Result<PathBuf> {
        download(
            &self.client,
            &a.url,
            dir,
            &format!("pexels_{}", safe_stem(&a.id)),
            &ext_of(
                &a.url,
                if a.kind == AssetKind::Video {
                    "mp4"
                } else {
                    "jpg"
                },
            ),
        )
        .await
    }
}

pub fn parse_pexels(v: &serde_json::Value, kind: AssetKind) -> Vec<StockAsset> {
    let mut out = Vec::new();
    match kind {
        AssetKind::Video => {
            for it in v["videos"].as_array().into_iter().flatten() {
                // Prefer the mp4 closest to 1080 wide without going huge.
                let best = it["video_files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|f| f["file_type"].as_str() == Some("video/mp4"))
                    .min_by_key(|f| (f["width"].as_i64().unwrap_or(0) - 1080).abs());
                if let Some(f) = best {
                    out.push(StockAsset {
                        id: it["id"].to_string(),
                        url: f["link"].as_str().unwrap_or_default().to_string(),
                        kind,
                        width: f["width"].as_u64().unwrap_or(0) as u32,
                        height: f["height"].as_u64().unwrap_or(0) as u32,
                        credit: format!(
                            "Video by {} on Pexels",
                            it["user"]["name"].as_str().unwrap_or("unknown")
                        ),
                    });
                }
            }
        }
        AssetKind::Image => {
            for it in v["photos"].as_array().into_iter().flatten() {
                if let Some(url) = it["src"]["large2x"]
                    .as_str()
                    .or_else(|| it["src"]["large"].as_str())
                {
                    out.push(StockAsset {
                        id: it["id"].to_string(),
                        url: url.to_string(),
                        kind,
                        width: it["width"].as_u64().unwrap_or(0) as u32,
                        height: it["height"].as_u64().unwrap_or(0) as u32,
                        credit: format!(
                            "Photo by {} on Pexels",
                            it["photographer"].as_str().unwrap_or("unknown")
                        ),
                    });
                }
            }
        }
    }
    out.retain(|a| !a.url.is_empty());
    out
}

/// Pixabay (https://pixabay.com/api/docs/).
#[derive(Debug, Clone)]
pub struct Pixabay {
    key: String,
    base: String,
    client: reqwest::Client,
}

impl Pixabay {
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            base: "https://pixabay.com".into(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }
}

#[async_trait]
impl StockProvider for Pixabay {
    fn name(&self) -> &str {
        "pixabay"
    }

    async fn search(&self, query: &str, kind: AssetKind, limit: usize) -> Result<Vec<StockAsset>> {
        let path = if kind == AssetKind::Video {
            "api/videos/"
        } else {
            "api/"
        };
        let resp = self
            .client
            .get(format!("{}/{path}", self.base))
            .query(&[
                ("key", self.key.as_str()),
                ("q", query),
                ("per_page", &limit.max(3).to_string()),
                ("safesearch", "true"),
            ])
            .send()
            .await
            .map_err(|e| StudioError::Http(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(StudioError::Http(format!("pixabay {}", resp.status())));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| StudioError::Http(e.to_string()))?;
        Ok(parse_pixabay(&v, kind))
    }

    async fn fetch(&self, a: &StockAsset, dir: &Path) -> Result<PathBuf> {
        download(
            &self.client,
            &a.url,
            dir,
            &format!("pixabay_{}", safe_stem(&a.id)),
            &ext_of(
                &a.url,
                if a.kind == AssetKind::Video {
                    "mp4"
                } else {
                    "jpg"
                },
            ),
        )
        .await
    }
}

pub fn parse_pixabay(v: &serde_json::Value, kind: AssetKind) -> Vec<StockAsset> {
    let mut out = Vec::new();
    for it in v["hits"].as_array().into_iter().flatten() {
        let credit = format!("{} on Pixabay", it["user"].as_str().unwrap_or("unknown"));
        match kind {
            AssetKind::Video => {
                let f = &it["videos"];
                let pick = ["large", "medium", "small"]
                    .iter()
                    .map(|k| &f[*k])
                    .find(|x| x["url"].as_str().is_some_and(|u| !u.is_empty()));
                if let Some(x) = pick {
                    out.push(StockAsset {
                        id: it["id"].to_string(),
                        url: x["url"].as_str().unwrap_or_default().to_string(),
                        kind,
                        width: x["width"].as_u64().unwrap_or(0) as u32,
                        height: x["height"].as_u64().unwrap_or(0) as u32,
                        credit,
                    });
                }
            }
            AssetKind::Image => {
                if let Some(url) = it["largeImageURL"].as_str() {
                    out.push(StockAsset {
                        id: it["id"].to_string(),
                        url: url.to_string(),
                        kind,
                        width: it["imageWidth"].as_u64().unwrap_or(0) as u32,
                        height: it["imageHeight"].as_u64().unwrap_or(0) as u32,
                        credit,
                    });
                }
            }
        }
    }
    out
}

/// The user's own footage / image folder, matched by file-name words.
#[derive(Debug, Clone)]
pub struct LocalFolder {
    dir: PathBuf,
}

impl LocalFolder {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

#[async_trait]
impl StockProvider for LocalFolder {
    fn name(&self) -> &str {
        "local"
    }

    async fn search(&self, query: &str, kind: AssetKind, limit: usize) -> Result<Vec<StockAsset>> {
        let words: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let mut hits: Vec<(usize, StockAsset)> = Vec::new();
        let mut rd = tokio::fs::read_dir(&self.dir).await?;
        while let Some(e) = rd.next_entry().await? {
            let path = e.path();
            let ext = path
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_lowercase();
            let k = match ext.as_str() {
                "mp4" | "mov" | "mkv" | "webm" => AssetKind::Video,
                "jpg" | "jpeg" | "png" | "webp" => AssetKind::Image,
                _ => continue,
            };
            if k != kind {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_lowercase();
            let score = words.iter().filter(|w| name.contains(w.as_str())).count();
            if score > 0 {
                hits.push((
                    score,
                    StockAsset {
                        id: name.clone(),
                        url: path.to_string_lossy().into_owned(),
                        kind: k,
                        width: 0,
                        height: 0,
                        credit: String::new(),
                    },
                ));
            }
        }
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
        Ok(hits.into_iter().take(limit).map(|h| h.1).collect())
    }

    async fn fetch(&self, a: &StockAsset, _dir: &Path) -> Result<PathBuf> {
        Ok(PathBuf::from(&a.url))
    }
}

/// Search + download for each request; requests with no hit are skipped.
pub async fn fetch_broll(
    providers: &[&dyn StockProvider],
    reqs: &[BrollRequest],
    dir: &Path,
) -> Vec<BrollPlacement> {
    let mut out = Vec::new();
    for r in reqs {
        'prov: for p in providers {
            for kind in [AssetKind::Video, AssetKind::Image] {
                let Ok(hits) = p.search(&r.query, kind, 5).await else {
                    continue;
                };
                // Prefer portrait-ish assets when sizes are known.
                let pick = hits
                    .iter()
                    .find(|a| a.height >= a.width && a.height > 0)
                    .or_else(|| hits.first());
                let Some(asset) = pick else { continue };
                match p.fetch(asset, dir).await {
                    Ok(path) => {
                        out.push(BrollPlacement {
                            start: r.start,
                            end: r.end,
                            query: r.query.clone(),
                            asset: path,
                            kind,
                            credit: (!asset.credit.is_empty()).then(|| asset.credit.clone()),
                        });
                        break 'prov;
                    }
                    Err(e) => {
                        tracing::warn!(provider = p.name(), error = %e, "b-roll fetch failed")
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ScriptedLlm;

    fn words(n: usize) -> Vec<CapWord> {
        (0..n)
            .map(|i| CapWord {
                text: format!("w{i}"),
                start: i as f64 * 0.5,
                end: i as f64 * 0.5 + 0.4,
            })
            .collect()
    }

    #[tokio::test]
    async fn plans_and_sanitizes_moments() {
        let reply = r#"{"moments":[
            {"start":0.2,"end":2.0,"query":"too early"},
            {"start":3.0,"end":9.0,"query":" city skyline "},
            {"start":4.0,"end":6.0,"query":"overlaps previous"},
            {"start":15.0,"end":16.0,"query":"too short"},
            {"start":12.0,"end":14.0,"query":""}
        ]}"#;
        let llm = ScriptedLlm::new([reply]);
        let out = plan_broll(&llm, &words(40), &BrollOpts::default())
            .await
            .unwrap();
        // "too early" moves to lead_in (2.5) -> 2.5..2.0 invalid; city skyline capped to 4s.
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].query, "city skyline");
        assert!((out[0].end - out[0].start - 4.0).abs() < 1e-9);
        assert!(
            plan_broll(&llm, &words(2), &BrollOpts::default())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn parses_pexels_video_and_image() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"videos":[{"id":7,"user":{"name":"Ann"},"video_files":[
                {"link":"http://x/a.mp4","width":640,"height":360,"file_type":"video/mp4"},
                {"link":"http://x/b.mp4","width":1080,"height":1920,"file_type":"video/mp4"},
                {"link":"http://x/c.webm","width":1080,"height":1920,"file_type":"video/webm"}]}]}"#,
        )
        .unwrap();
        let a = parse_pexels(&v, AssetKind::Video);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].url, "http://x/b.mp4");
        assert!(a[0].credit.contains("Ann"));
        let p: serde_json::Value = serde_json::from_str(r#"{"photos":[{"id":1,"photographer":"Bo","width":10,"height":20,"src":{"large":"http://x/p.jpg"}}]}"#).unwrap();
        assert_eq!(parse_pexels(&p, AssetKind::Image)[0].url, "http://x/p.jpg");
    }

    #[test]
    fn parses_pixabay() {
        let v: serde_json::Value = serde_json::from_str(r#"{"hits":[{"id":3,"user":"Cy","videos":{"large":{"url":""},"medium":{"url":"http://x/m.mp4","width":1280,"height":720}}}]}"#).unwrap();
        let a = parse_pixabay(&v, AssetKind::Video);
        assert_eq!(a[0].url, "http://x/m.mp4");
        assert_eq!(ext_of("http://x/m.mp4?x=1", "bin"), "mp4");
        assert_eq!(ext_of("http://x/noext", "bin"), "bin");
    }

    #[tokio::test]
    async fn local_folder_matches_by_name() {
        let d = std::env::temp_dir().join(format!("hk_broll_{}", crate::project::new_id("t")));
        tokio::fs::create_dir_all(&d).await.unwrap();
        tokio::fs::write(d.join("city_skyline_night.mp4"), b"x")
            .await
            .unwrap();
        tokio::fs::write(d.join("forest.mp4"), b"x").await.unwrap();
        tokio::fs::write(d.join("notes.txt"), b"x").await.unwrap();
        let p = LocalFolder::new(&d);
        let hits = p.search("city skyline", AssetKind::Video, 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        let placements = fetch_broll(
            &[&p],
            &[
                BrollRequest {
                    start: 3.0,
                    end: 5.0,
                    query: "forest".into(),
                },
                BrollRequest {
                    start: 9.0,
                    end: 11.0,
                    query: "ocean".into(),
                },
            ],
            &d,
        )
        .await;
        assert_eq!(placements.len(), 1);
        assert!(placements[0].asset.ends_with("forest.mp4"));
        tokio::fs::remove_dir_all(&d).await.unwrap();
    }
}
