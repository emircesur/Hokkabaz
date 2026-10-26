//! Local knowledge bases (P9.10): index folders into line-ranged chunks and search them
//! with BM25, returning `file:start-end` citations. Embedding/hybrid ranking comes later.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::textindex::bm25;
use super::{base_dir, fail, looks_binary, parse};
use crate::tool::{Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolSpec};

/// Text embedder for hybrid search (set by the host from its generation settings).
pub trait Embedder: Send + Sync {
    fn embed(
        &self,
        texts: Vec<String>,
    ) -> futures::future::BoxFuture<'_, Result<Vec<Vec<f32>>, String>>;
}

static EMBEDDER: std::sync::RwLock<Option<Arc<dyn Embedder>>> = std::sync::RwLock::new(None);

/// Enable (or disable with `None`) embedding-based hybrid search for knowledge bases.
pub fn set_embedder(e: Option<Arc<dyn Embedder>>) {
    *EMBEDDER
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = e;
}

fn embedder() -> Option<Arc<dyn Embedder>> {
    EMBEDDER
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut d, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        d += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        d / (na.sqrt() * nb.sqrt())
    }
}

const CHUNK_LINES: usize = 40;
const OVERLAP: usize = 5;
const MAX_FILE_BYTES: u64 = 1_000_000;
const MAX_CHUNKS: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Base {
    name: String,
    root: String,
    chunks: Vec<Chunk>,
}

/// Knowledge bases stored as one JSON file each under `<dir>/knowledge/`.
#[derive(Debug)]
pub struct KnowledgeBase {
    dir: PathBuf,
}

fn safe_name(n: &str) -> String {
    n.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Split text into overlapping line windows.
pub fn chunk_text(file: &str, text: &str) -> Vec<Chunk> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let end = (i + CHUNK_LINES).min(lines.len());
        let body = lines[i..end].join("\n");
        if !body.trim().is_empty() {
            out.push(Chunk {
                file: file.to_string(),
                start_line: i + 1,
                end_line: end,
                text: body,
            });
        }
        if end == lines.len() {
            break;
        }
        i = end - OVERLAP;
    }
    out
}

impl KnowledgeBase {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into().join("knowledge"),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.json", safe_name(name)))
    }

    fn load(&self, name: &str) -> Option<Base> {
        serde_json::from_slice(&std::fs::read(self.path(name)).ok()?).ok()
    }

    /// (Re)index `root` (gitignore-aware) as base `name`. Returns (files, chunks).
    pub fn index(&self, name: &str, root: &Path) -> std::io::Result<(usize, usize)> {
        let mut base = Base {
            name: name.to_string(),
            root: root.display().to_string(),
            chunks: Vec::new(),
        };
        let mut files = 0;
        for e in ignore::WalkBuilder::new(root).build().flatten() {
            let p = e.path();
            if !e.file_type().is_some_and(|t| t.is_file())
                || e.metadata().map_or(true, |m| m.len() > MAX_FILE_BYTES)
            {
                continue;
            }
            let Ok(bytes) = std::fs::read(p) else {
                continue;
            };
            if looks_binary(&bytes) {
                continue;
            }
            let rel = p.strip_prefix(root).unwrap_or(p).display().to_string();
            base.chunks
                .extend(chunk_text(&rel, &String::from_utf8_lossy(&bytes)));
            files += 1;
            if base.chunks.len() >= MAX_CHUNKS {
                break;
            }
        }
        std::fs::create_dir_all(&self.dir)?;
        let n = base.chunks.len();
        std::fs::write(self.path(name), serde_json::to_vec(&base)?)?;
        Ok((files, n))
    }

    pub fn list(&self) -> Vec<Value> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        rd.flatten()
            .filter_map(|e| serde_json::from_slice::<Base>(&std::fs::read(e.path()).ok()?).ok())
            .map(|b| json!({"name": b.name, "root": b.root, "chunks": b.chunks.len()}))
            .collect()
    }

    pub fn remove(&self, name: &str) -> bool {
        let _ = std::fs::remove_file(self.vec_path(name));
        std::fs::remove_file(self.path(name)).is_ok()
    }

    fn vec_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.vec.json", safe_name(name)))
    }

    /// Index, then embed every chunk when an embedder is configured. Returns (files, chunks, embedded).
    pub async fn index_hybrid(
        self: &Arc<Self>,
        name: &str,
        root: &Path,
    ) -> Result<(usize, usize, bool), String> {
        let (kb, n, r) = (self.clone(), name.to_string(), root.to_path_buf());
        let (files, chunks) = tokio::task::spawn_blocking(move || kb.index(&n, &r))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let Some(emb) = embedder() else {
            let _ = std::fs::remove_file(self.vec_path(name));
            return Ok((files, chunks, false));
        };
        let base = self.load(name).ok_or("index vanished")?;
        let mut vecs: Vec<Vec<f32>> = Vec::with_capacity(base.chunks.len());
        for batch in base.chunks.chunks(32) {
            let texts: Vec<String> = batch
                .iter()
                .map(|c| c.text.chars().take(2000).collect())
                .collect();
            vecs.extend(emb.embed(texts).await?);
        }
        std::fs::write(
            self.vec_path(name),
            serde_json::to_vec(&vecs).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok((files, chunks, true))
    }

    /// BM25 + cosine (when vectors and an embedder exist), else BM25 only.
    pub async fn search_hybrid(
        &self,
        query: &str,
        bases: &[String],
        k: usize,
    ) -> Vec<(String, Chunk)> {
        let Some(emb) = embedder() else {
            return self.search(query, bases, k);
        };
        let names: Vec<String> = if bases.is_empty() {
            self.list()
                .iter()
                .filter_map(|v| v["name"].as_str().map(str::to_string))
                .collect()
        } else {
            bases.to_vec()
        };
        let Ok(qv) = emb.embed(vec![query.to_string()]).await else {
            return self.search(query, bases, k);
        };
        let Some(qv) = qv.into_iter().next() else {
            return self.search(query, bases, k);
        };
        let mut scored: Vec<(f32, String, Chunk)> = Vec::new();
        for n in names {
            let Some(base) = self.load(&n) else { continue };
            let vecs: Vec<Vec<f32>> = std::fs::read(self.vec_path(&n))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let docs: Vec<&str> = base.chunks.iter().map(|c| c.text.as_str()).collect();
            let lex = bm25(query, &docs, docs.len());
            let max = lex.first().map_or(1.0, |x| x.1.max(1e-9));
            let lex_of: std::collections::HashMap<usize, f64> = lex.into_iter().collect();
            for (i, c) in base.chunks.iter().enumerate() {
                let l = lex_of.get(&i).copied().unwrap_or(0.0) / max;
                let v = vecs.get(i).map_or(0.0, |v| cosine(&qv, v));
                let score = 0.5 * l as f32 + 0.5 * v.max(0.0);
                if score > 0.05 {
                    scored.push((score, n.clone(), c.clone()));
                }
            }
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.into_iter().take(k).map(|(_, n, c)| (n, c)).collect()
    }

    /// Top `k` chunks for `query` across `bases` (all when empty).
    pub fn search(&self, query: &str, bases: &[String], k: usize) -> Vec<(String, Chunk)> {
        let names: Vec<String> = if bases.is_empty() {
            self.list()
                .iter()
                .filter_map(|v| v["name"].as_str().map(str::to_string))
                .collect()
        } else {
            bases.to_vec()
        };
        let loaded: Vec<Base> = names.iter().filter_map(|n| self.load(n)).collect();
        let all: Vec<(&str, &Chunk)> = loaded
            .iter()
            .flat_map(|b| b.chunks.iter().map(move |c| (b.name.as_str(), c)))
            .collect();
        let docs: Vec<&str> = all.iter().map(|(_, c)| c.text.as_str()).collect();
        bm25(query, &docs, k)
            .into_iter()
            .map(|(i, _)| (all[i].0.to_string(), all[i].1.clone()))
            .collect()
    }
}

/// `knowledge` tool: index / search / list / remove.
#[derive(Debug)]
pub struct KnowledgeTool(pub Arc<KnowledgeBase>);

#[derive(Deserialize)]
struct Args {
    action: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    bases: Vec<String>,
    #[serde(default)]
    k: Option<usize>,
}

#[async_trait]
impl Tool for KnowledgeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "knowledge".into(),
            description: "Local knowledge bases. `index` a folder under a name, `search` them \
                (returns file:line citations — cite them in answers), `list`, `remove`."
                .into(),
            parameters: json!({"type":"object","properties":{
                "action":{"type":"string","enum":["index","search","list","remove"]},
                "name":{"type":"string"},"path":{"type":"string"},"query":{"type":"string"},
                "bases":{"type":"array","items":{"type":"string"}},
                "k":{"type":"integer","minimum":1,"maximum":20}},
                "required":["action"],"additionalProperties":false}),
        }
    }

    fn risk(&self, _: &Value) -> Risk {
        Risk::Safe
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let a: Args = parse("knowledge", args)?;
        let kb = self.0.clone();
        let root = base_dir(&ctx, a.path.as_deref());
        let res = async move {
            match a.action.as_str() {
                "index" => {
                    let name = a.name.ok_or("index needs `name`")?;
                    let (files, chunks, embedded) = kb.index_hybrid(&name, &root).await?;
                    Ok(json!({"name": name, "files": files, "chunks": chunks, "embedded": embedded}))
                }
                "search" => {
                    let q = a.query.ok_or("search needs `query`")?;
                    let hits: Vec<Value> = kb
                        .search_hybrid(&q, &a.bases, a.k.unwrap_or(5))
                        .await
                        .into_iter()
                        .map(|(b, c)| {
                            let snippet: String = c.text.chars().take(800).collect();
                            json!({"base": b, "cite": format!("{}:{}-{}", c.file, c.start_line, c.end_line), "text": snippet})
                        })
                        .collect();
                    Ok(json!(hits))
                }
                "list" => Ok(json!(kb.list())),
                "remove" => Ok(json!({"removed": kb.remove(&a.name.ok_or("remove needs `name`")?)})),
                other => Err(format!("unknown action `{other}`")),
            }
        }
        .await;
        match res {
            Ok(v) => Ok(ToolOutput::ok(v.to_string())),
            Err(e) => fail(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_cite() {
        let root = std::env::temp_dir().join(format!("hk_kb_src_{}", std::process::id()));
        let data = std::env::temp_dir().join(format!("hk_kb_data_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut body: Vec<String> = (0..60).map(|i| format!("line {i}")).collect();
        body[50] = "the webhook signature uses HMAC sha256".into();
        std::fs::write(root.join("doc.md"), body.join("\n")).unwrap();
        let kb = KnowledgeBase::new(&data);
        assert_eq!(kb.index("docs", &root).unwrap(), (1, 2));
        let hits = kb.search("hmac webhook", &[], 3);
        assert_eq!(hits[0].1.start_line, 36);
        assert!(hits[0].1.text.contains("HMAC"));
        assert!(kb.remove("docs"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&data);
    }
}
