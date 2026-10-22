//! Local file nodes: Read/Write Files from Disk (`readWriteFile`) and Spreadsheet File
//! (`spreadsheetFile`, CSV only; XLSX/ODS/XLS fail with a clear error).
//!
//! Disk access is limited to allowed roots (home, temp, plus `HOKKABAZ_FILE_ROOTS`) and the nodes
//! are off on Android, where flows have no filesystem of their own to touch.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Map, Value, json};

use super::more::{csv_cell, make_binary, parse_csv, with_binary};
use super::util::*;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::Item;
use crate::node::{FlowNode, NodeContext, NodeOutput};

#[derive(Debug)]
pub struct ReadWriteFileNode(NodeTypeDescription);

impl Default for ReadWriteFileNode {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadWriteFileNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Read/Write Files from Disk",
            "name": "n8n-nodes-base.readWriteFile",
            "version": [1],
            "description": "Read or write files on the machine the flow runs on",
            "group": ["input"],
            "defaults": {"name": "Read/Write Files from Disk"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Operation", "name": "operation", "type": "options", "default": "read",
                 "options": [{"name": "Read File(s) From Disk", "value": "read"}, {"name": "Write File to Disk", "value": "write"}]},
                {"displayName": "File(s) Selector", "name": "fileSelector", "type": "string", "default": "",
                 "placeholder": "/data/example.jpg  or  /data/*.csv", "displayOptions": {"show": {"operation": ["read"]}}},
                {"displayName": "File Path and Name", "name": "fileName", "type": "string", "default": "",
                 "displayOptions": {"show": {"operation": ["write"]}}},
                {"displayName": "Input Binary Field", "name": "dataPropertyName", "type": "string", "default": "data",
                 "displayOptions": {"show": {"operation": ["write"]}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {}, "placeholder": "Add option", "options": [
                    {"displayName": "Put Output File in Field", "name": "dataPropertyName", "type": "string", "default": "data"},
                    {"displayName": "Append", "name": "append", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

fn roots() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if let Some(extra) = std::env::var_os("HOKKABAZ_FILE_ROOTS") {
        v.extend(std::env::split_paths(&extra));
    }
    for k in ["HOME", "USERPROFILE"] {
        if let Some(h) = std::env::var_os(k) {
            v.push(h.into());
        }
    }
    v.push(std::env::temp_dir());
    v.into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect()
}

/// Resolve to an absolute path inside an allowed root; `..` and symlinks can't escape because
/// the check runs on the canonical path (for writes: the canonical parent).
fn checked(path: &Path, must_exist: bool) -> Result<PathBuf, String> {
    let canon = if must_exist {
        path.canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?
    } else {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = path
            .file_name()
            .ok_or_else(|| format!("{}: no file name", path.display()))?;
        parent
            .canonicalize()
            .map_err(|e| format!("{}: {e}", parent.display()))?
            .join(name)
    };
    if roots().iter().any(|r| canon.starts_with(r)) {
        Ok(canon)
    } else {
        Err(format!(
            "{} is outside the folders flows may access (your home, the temp folder, HOKKABAZ_FILE_ROOTS)",
            canon.display()
        ))
    }
}

/// `*` and `?` in the file-name part only.
fn glob_match(pat: &[char], name: &[char]) -> bool {
    match (pat.first(), name.first()) {
        (None, None) => true,
        (Some('*'), _) => {
            glob_match(&pat[1..], name) || (!name.is_empty() && glob_match(pat, &name[1..]))
        }
        (Some('?'), Some(_)) => glob_match(&pat[1..], &name[1..]),
        (Some(a), Some(b)) if a == b => glob_match(&pat[1..], &name[1..]),
        _ => false,
    }
}

fn mime_for(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "csv" => "text/csv",
        "txt" | "log" | "md" => "text/plain",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

#[async_trait]
impl FlowNode for ReadWriteFileNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        #[cfg(target_os = "android")]
        {
            Err(node_err(
                ctx,
                "Read/Write Files from Disk is not available on Android",
            ))
        }
        #[cfg(not(target_os = "android"))]
        {
            let op = param_str(ctx, "operation", 0, "read")?;
            let mut out = Vec::new();
            if op == "read" {
                let items = port(ctx, 0);
                let n = items.len().max(1);
                for i in 0..n {
                    let sel = param_str(ctx, "fileSelector", i, "")?;
                    let prop = param_str(ctx, "options.dataPropertyName", i, "data")?;
                    if sel.is_empty() {
                        return Err(node_err(ctx, "File(s) Selector is empty"));
                    }
                    let p = PathBuf::from(&sel);
                    let fname = p
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let files: Vec<PathBuf> = if fname.contains(['*', '?']) {
                        let dir = p
                            .parent()
                            .filter(|d| !d.as_os_str().is_empty())
                            .unwrap_or(Path::new("."));
                        let dir = checked(dir, true).map_err(|e| node_err(ctx, e))?;
                        let pat: Vec<char> = fname.chars().collect();
                        let mut found = Vec::new();
                        let rd = tokio::fs::read_dir(&dir)
                            .await
                            .map_err(|e| node_err(ctx, format!("{}: {e}", dir.display())))?;
                        let mut rd = rd;
                        while let Ok(Some(e)) = rd.next_entry().await {
                            let n: Vec<char> = e.file_name().to_string_lossy().chars().collect();
                            if glob_match(&pat, &n)
                                && e.file_type().await.is_ok_and(|t| t.is_file())
                            {
                                found.push(e.path());
                            }
                        }
                        found.sort();
                        found
                    } else {
                        vec![checked(&p, true).map_err(|e| node_err(ctx, e))?]
                    };
                    for f in files {
                        let bytes = tokio::fs::read(&f)
                            .await
                            .map_err(|e| node_err(ctx, format!("{}: {e}", f.display())))?;
                        let ext = f
                            .extension()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let name = f
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let mut b = make_binary(&bytes, mime_for(&ext), &name, &ext);
                        b.directory = f.parent().map(|d| d.to_string_lossy().into_owned());
                        let mut o = with_binary(&prop, b);
                        o.json.insert("fileName".into(), json!(f.to_string_lossy()));
                        o.paired_item = paired(i);
                        out.push(o);
                    }
                }
            } else {
                for (i, it) in port(ctx, 0).iter().enumerate() {
                    let path = param_str(ctx, "fileName", i, "")?;
                    let prop = param_str(ctx, "dataPropertyName", i, "data")?;
                    let append = param_bool(ctx, "options.append", i, false)?;
                    if path.is_empty() {
                        return Err(node_err(ctx, "File Path and Name is empty"));
                    }
                    let bin = it
                        .binary
                        .as_ref()
                        .and_then(|b| b.get(&prop))
                        .ok_or_else(|| {
                            node_err(ctx, format!("Item {i} has no binary field '{prop}'"))
                        })?;
                    let bytes = B64.decode(bin.data.as_bytes()).map_err(|e| {
                        node_err(ctx, format!("Binary data is not valid base64: {e}"))
                    })?;
                    let target = checked(Path::new(&path), false).map_err(|e| node_err(ctx, e))?;
                    if append {
                        use tokio::io::AsyncWriteExt as _;
                        let mut f = tokio::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&target)
                            .await
                            .map_err(|e| node_err(ctx, format!("{}: {e}", target.display())))?;
                        f.write_all(&bytes)
                            .await
                            .map_err(|e| node_err(ctx, e.to_string()))?;
                    } else {
                        tokio::fs::write(&target, &bytes)
                            .await
                            .map_err(|e| node_err(ctx, format!("{}: {e}", target.display())))?;
                    }
                    let mut o = it.clone();
                    o.json
                        .insert("fileName".into(), json!(target.to_string_lossy()));
                    o.paired_item = paired(i);
                    out.push(o);
                }
            }
            Ok(vec![out])
        }
    }
}

// ------------------------------------------------------------- Spreadsheet File

#[derive(Debug)]
pub struct SpreadsheetFileNode(NodeTypeDescription);

impl Default for SpreadsheetFileNode {
    fn default() -> Self {
        Self::new()
    }
}

impl SpreadsheetFileNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Spreadsheet File",
            "name": "n8n-nodes-base.spreadsheetFile",
            "version": [1, 2],
            "description": "Read and write CSV spreadsheet files (XLSX/ODS are not supported)",
            "group": ["input"],
            "defaults": {"name": "Spreadsheet File"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Operation", "name": "operation", "type": "options", "default": "fromFile",
                 "options": [{"name": "Read From File", "value": "fromFile"}, {"name": "Write to File", "value": "toFile"}]},
                {"displayName": "File Format", "name": "fileFormat", "type": "options", "default": "csv",
                 "displayOptions": {"show": {"operation": ["toFile"]}},
                 "options": [{"name": "CSV", "value": "csv"}, {"name": "XLSX (not supported)", "value": "xlsx"}]},
                {"displayName": "Input Binary Field", "name": "binaryPropertyName", "type": "string", "default": "data",
                 "displayOptions": {"show": {"operation": ["fromFile"]}}},
                {"displayName": "Put Output File in Field", "name": "binaryPropertyName", "type": "string", "default": "data",
                 "displayOptions": {"show": {"operation": ["toFile"]}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {}, "placeholder": "Add option", "options": [
                    {"displayName": "Delimiter", "name": "delimiter", "type": "string", "default": ","},
                    {"displayName": "Header Row", "name": "headerRow", "type": "boolean", "default": true},
                    {"displayName": "File Name", "name": "fileName", "type": "string", "default": ""}]}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for SpreadsheetFileNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let op = param_str(ctx, "operation", 0, "fromFile")?;
        let prop = param_str(ctx, "binaryPropertyName", 0, "data")?;
        let delim = param_str(ctx, "options.delimiter", 0, ",")?
            .chars()
            .next()
            .unwrap_or(',');
        let header = param_bool(ctx, "options.headerRow", 0, true)?;
        let items = port(ctx, 0);
        let mut out = Vec::new();
        if op == "toFile" {
            let fmt = param_str(ctx, "fileFormat", 0, "csv")?;
            if fmt != "csv" {
                return Err(node_err(
                    ctx,
                    format!("Spreadsheet format '{fmt}' is not supported; use CSV"),
                ));
            }
            if items.is_empty() {
                return Ok(vec![out]);
            }
            let mut cols: Vec<String> = Vec::new();
            for it in items {
                for k in it.json.keys() {
                    if !cols.contains(k) {
                        cols.push(k.clone());
                    }
                }
            }
            let d = delim.to_string();
            let mut lines = Vec::new();
            if header {
                lines.push(
                    cols.iter()
                        .map(|c| csv_cell(&Value::String(c.clone()), delim))
                        .collect::<Vec<_>>()
                        .join(&d),
                );
            }
            for it in items {
                lines.push(
                    cols.iter()
                        .map(|c| csv_cell(it.json.get(c).unwrap_or(&Value::Null), delim))
                        .collect::<Vec<_>>()
                        .join(&d),
                );
            }
            let name = param_str(ctx, "options.fileName", 0, "spreadsheet.csv")?;
            out.push(with_binary(
                &prop,
                make_binary(lines.join("\n").as_bytes(), "text/csv", &name, "csv"),
            ));
            return Ok(vec![out]);
        }
        for (i, it) in items.iter().enumerate() {
            let bin = it
                .binary
                .as_ref()
                .and_then(|b| b.get(&prop))
                .ok_or_else(|| node_err(ctx, format!("Item {i} has no binary field '{prop}'")))?;
            let is_csv =
                bin.file_extension.as_deref().is_some_and(|e| {
                    e.eq_ignore_ascii_case("csv") || e.eq_ignore_ascii_case("txt")
                }) || bin.mime_type.contains("csv")
                    || bin.mime_type.starts_with("text/");
            if !is_csv {
                return Err(node_err(
                    ctx,
                    format!(
                        "'{}' is not a CSV file; XLSX/XLS/ODS are not supported",
                        bin.file_name.as_deref().unwrap_or(&prop)
                    ),
                ));
            }
            let bytes = B64
                .decode(bin.data.as_bytes())
                .map_err(|e| node_err(ctx, format!("Binary data is not valid base64: {e}")))?;
            let mut rows = parse_csv(&String::from_utf8_lossy(&bytes), delim);
            rows.retain(|r| !(r.len() == 1 && r[0].is_empty()));
            let head: Vec<String> = if header && !rows.is_empty() {
                rows.remove(0)
            } else {
                rows.first()
                    .map(|r| (0..r.len()).map(|k| k.to_string()).collect())
                    .unwrap_or_default()
            };
            for r in rows {
                let mut m = Map::new();
                for (k, h) in head.iter().enumerate() {
                    m.insert(
                        h.clone(),
                        Value::String(r.get(k).cloned().unwrap_or_default()),
                    );
                }
                let mut o = Item::new(m);
                o.paired_item = paired(i);
                out.push(o);
            }
        }
        Ok(vec![out])
    }
}
