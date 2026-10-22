//! Smaller core nodes real workflows lean on: Sticky Note, Stop and Error, Summarize, Rename
//! Keys, Compare Datasets, Convert to File, Extract from File, the legacy Function / Function
//! Item code nodes.
//!
//! Clean-room, from the public n8n docs and exported JSON. Spreadsheet formats other than CSV,
//! PDF and HTML-table extraction are not implemented; those operations fail with a clear error.

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use super::util::*;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::expr::ScriptMode;
use crate::item::{BinaryData, Item};
use crate::node::{FlowNode, NodeContext, NodeOutput};

macro_rules! simple_node {
    ($name:ident) => {
        #[derive(Debug)]
        pub struct $name(NodeTypeDescription);
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

// ------------------------------------------------------------------ Sticky Note

simple_node!(StickyNoteNode);

impl StickyNoteNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Sticky Note",
            "name": "n8n-nodes-base.stickyNote",
            "version": [1],
            "description": "Make your workflow easier to understand",
            "group": ["input"],
            "defaults": {"name": "Sticky Note"},
            "inputs": [],
            "outputs": [],
            "hidden": true,
            "properties": [
                {"displayName": "Content", "name": "content", "type": "string", "default": ""},
                {"displayName": "Height", "name": "height", "type": "number", "default": 160},
                {"displayName": "Width", "name": "width", "type": "number", "default": 240},
                {"displayName": "Color", "name": "color", "type": "number", "default": 1}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for StickyNoteNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, _ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        Ok(Vec::new())
    }
}

// --------------------------------------------------------------- Stop and Error

simple_node!(StopAndErrorNode);

impl StopAndErrorNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Stop and Error",
            "name": "n8n-nodes-base.stopAndError",
            "version": [1],
            "description": "Throw an error in the workflow",
            "group": ["input"],
            "defaults": {"name": "Stop and Error", "color": "#ff0000"},
            "inputs": ["main"],
            "outputs": [],
            "properties": [
                {"displayName": "Error Type", "name": "errorType", "type": "options", "default": "errorMessage",
                 "options": [{"name": "Error Message", "value": "errorMessage"}, {"name": "Error Object", "value": "errorObject"}]},
                {"displayName": "Error Message", "name": "errorMessage", "type": "string", "default": "",
                 "displayOptions": {"show": {"errorType": ["errorMessage"]}}},
                {"displayName": "Error Object", "name": "errorObject", "type": "json", "default": "",
                 "displayOptions": {"show": {"errorType": ["errorObject"]}}}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for StopAndErrorNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let msg = if param_str(ctx, "errorType", 0, "errorMessage")? == "errorObject" {
            match param(ctx, "errorObject", 0)? {
                Some(Value::String(s)) => serde_json::from_str::<Value>(&s)
                    .ok()
                    .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
                    .unwrap_or(s),
                Some(v) => v
                    .get("message")
                    .and_then(Value::as_str)
                    .map_or_else(|| v.to_string(), str::to_owned),
                None => String::new(),
            }
        } else {
            param_str(ctx, "errorMessage", 0, "")?
        };
        Err(node_err(
            ctx,
            if msg.is_empty() {
                "Stopped by Stop and Error node"
            } else {
                &msg
            },
        ))
    }
}

// -------------------------------------------------------------------- Summarize

simple_node!(SummarizeNode);

impl SummarizeNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Summarize",
            "name": "n8n-nodes-base.summarize",
            "version": [1, 1.1],
            "description": "Sum, count, max... across items",
            "group": ["transform"],
            "defaults": {"name": "Summarize"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Fields to Summarize", "name": "fieldsToSummarize", "type": "fixedCollection",
                 "default": {"values": [{"aggregation": "count", "field": ""}]},
                 "typeOptions": {"multipleValues": true},
                 "options": [{"displayName": "Fields", "name": "values", "values": [
                    {"displayName": "Aggregation", "name": "aggregation", "type": "options", "default": "count",
                     "options": [
                        {"name": "Append", "value": "append"}, {"name": "Average", "value": "average"},
                        {"name": "Concatenate", "value": "concatenate"}, {"name": "Count", "value": "count"},
                        {"name": "Count Unique", "value": "countUnique"}, {"name": "Max", "value": "max"},
                        {"name": "Min", "value": "min"}, {"name": "Sum", "value": "sum"}]},
                    {"displayName": "Field", "name": "field", "type": "string", "default": ""},
                    {"displayName": "Include Empty Values", "name": "includeEmpty", "type": "boolean", "default": false},
                    {"displayName": "Separator", "name": "separateBy", "type": "options", "default": ",",
                     "options": [{"name": "Comma", "value": ","}, {"name": "Comma and Space", "value": ", "},
                                 {"name": "New Line", "value": "\n"}, {"name": "None", "value": ""},
                                 {"name": "Space", "value": " "}, {"name": "Other", "value": "other"}]},
                    {"displayName": "Custom Separator", "name": "customSeparator", "type": "string", "default": ""}
                 ]}]},
                {"displayName": "Fields to Split By", "name": "fieldsToSplitBy", "type": "string", "default": "",
                 "placeholder": "e.g. country, city"},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {}, "placeholder": "Add option",
                 "options": [
                    {"displayName": "Continue if Field Not Found", "name": "continueIfFieldNotFound", "type": "boolean", "default": false},
                    {"displayName": "Output Format", "name": "outputFormat", "type": "options", "default": "separateItems",
                     "options": [{"name": "Each Split in a Separate Item", "value": "separateItems"},
                                 {"name": "All Splits in a Single Item", "value": "singleItem"}]},
                    {"displayName": "Ignore Items Without Valid Fields to Split By", "name": "skipEmptySplitFields", "type": "boolean", "default": false}
                 ]}
            ]
        })))
    }
}

fn aggregate_one(values: &[&Value], agg: &str, include_empty: bool, sep: &str) -> Value {
    let is_empty = |v: &&Value| v.is_null() || matches!(v, Value::String(s) if s.is_empty());
    let pick: Vec<&Value> = values
        .iter()
        .copied()
        .filter(|v| include_empty || !is_empty(v))
        .collect();
    let nums = || {
        pick.iter().filter_map(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
    };
    match agg {
        "append" => Value::Array(pick.into_iter().cloned().collect()),
        "concatenate" => Value::String(
            pick.iter()
                .map(|v| to_display(v))
                .collect::<Vec<_>>()
                .join(sep),
        ),
        "count" => num(pick.len() as f64),
        "countUnique" => num(pick
            .iter()
            .map(|v| canonical(v))
            .collect::<std::collections::HashSet<_>>()
            .len() as f64),
        "sum" => num(nums().sum()),
        "average" => {
            let v: Vec<f64> = nums().collect();
            if v.is_empty() {
                Value::Null
            } else {
                num(v.iter().sum::<f64>() / v.len() as f64)
            }
        }
        "max" => nums()
            .fold(None, |a: Option<f64>, x| Some(a.map_or(x, |a| a.max(x))))
            .map_or(Value::Null, num),
        "min" => nums()
            .fold(None, |a: Option<f64>, x| Some(a.map_or(x, |a| a.min(x))))
            .map_or(Value::Null, num),
        _ => Value::Null,
    }
}

#[async_trait]
impl FlowNode for SummarizeNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let items = port(ctx, 0);
        let specs = raw_array(ctx, "fieldsToSummarize.values");
        if specs.is_empty() {
            return Err(node_err(ctx, "Add at least one field to summarize"));
        }
        let split = param_list(ctx, "fieldsToSplitBy", 0)?;
        let skip_empty = param_bool(ctx, "options.skipEmptySplitFields", 0, false)?;
        let single = param_str(ctx, "options.outputFormat", 0, "separateItems")? == "singleItem";
        let lenient = param_bool(ctx, "options.continueIfFieldNotFound", 0, false)?;

        let mut groups: IndexMap<String, Vec<&Item>> = IndexMap::new();
        for it in items {
            let key: Vec<Value> = split
                .iter()
                .map(|f| get_path(&it.json, f, true).cloned().unwrap_or(Value::Null))
                .collect();
            if skip_empty && key.iter().any(|v| v.is_null()) {
                continue;
            }
            groups
                .entry(canonical(&Value::Array(key)))
                .or_default()
                .push(it);
        }
        if split.is_empty() {
            groups.clear();
            groups.insert(String::new(), items.iter().collect());
        }

        let mut rows: Vec<Map<String, Value>> = Vec::new();
        for members in groups.values() {
            let mut row = Map::new();
            if let Some(first) = members.first() {
                for f in &split {
                    row.insert(
                        f.clone(),
                        get_path(&first.json, f, true)
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                }
            }
            for s in specs {
                let agg = s
                    .get("aggregation")
                    .and_then(Value::as_str)
                    .unwrap_or("count");
                let field = s.get("field").and_then(Value::as_str).unwrap_or("");
                let include_empty = s
                    .get("includeEmpty")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let sep = match s.get("separateBy").and_then(Value::as_str).unwrap_or(",") {
                    "other" => s
                        .get("customSeparator")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    v => v,
                };
                let vals: Vec<&Value> = members
                    .iter()
                    .filter_map(|it| get_path(&it.json, field, true))
                    .collect();
                if vals.is_empty() && !members.is_empty() && !lenient && agg != "count" {
                    return Err(node_err(
                        ctx,
                        format!("The field '{field}' does not exist in any items"),
                    ));
                }
                row.insert(
                    format!("{agg}_{field}"),
                    aggregate_one(&vals, agg, include_empty, sep),
                );
            }
            rows.push(row);
        }
        if single {
            let mut m = Map::new();
            m.insert(
                "splitBy".into(),
                Value::Array(rows.into_iter().map(Value::Object).collect()),
            );
            return Ok(vec![vec![Item::new(m)]]);
        }
        Ok(vec![rows.into_iter().map(Item::new).collect()])
    }
}

// ------------------------------------------------------------------ Rename Keys

simple_node!(RenameKeysNode);

impl RenameKeysNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Rename Keys",
            "name": "n8n-nodes-base.renameKeys",
            "version": [1],
            "description": "Update item field names",
            "group": ["transform"],
            "defaults": {"name": "Rename Keys", "color": "#772244"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Keys", "name": "keys", "type": "fixedCollection", "default": {"key": [{"currentKey": "", "newKey": ""}]},
                 "typeOptions": {"multipleValues": true},
                 "options": [{"displayName": "Key", "name": "key", "values": [
                    {"displayName": "Current Key Name", "name": "currentKey", "type": "string", "default": ""},
                    {"displayName": "New Key Name", "name": "newKey", "type": "string", "default": ""}]}]},
                {"displayName": "Additional Options", "name": "additionalOptions", "type": "collection", "default": {}, "placeholder": "Add option",
                 "options": [{"displayName": "Regex", "name": "regexReplacement", "placeholder": "Add new regular expression", "type": "fixedCollection",
                    "typeOptions": {"multipleValues": true}, "default": {},
                    "options": [{"displayName": "Replacement", "name": "replacements", "values": [
                        {"displayName": "Regex", "name": "searchRegex", "type": "string", "default": ""},
                        {"displayName": "Replace With", "name": "replaceRegex", "type": "string", "default": ""},
                        {"displayName": "Options", "name": "options", "type": "collection", "default": {}, "options": [
                            {"displayName": "Case Insensitive", "name": "caseInsensitive", "type": "boolean", "default": false},
                            {"displayName": "Max Depth", "name": "depth", "type": "number", "default": -1}]}]}]}]}
            ]
        })))
    }
}

fn rename_deep(
    m: &Map<String, Value>,
    re: &regex::Regex,
    with: &str,
    depth: i64,
) -> Map<String, Value> {
    m.iter()
        .map(|(k, v)| {
            let nk = re.replace_all(k, with).into_owned();
            let nv = match v {
                Value::Object(o) if depth != 0 => {
                    Value::Object(rename_deep(o, re, with, depth - 1))
                }
                other => other.clone(),
            };
            (nk, nv)
        })
        .collect()
}

#[async_trait]
impl FlowNode for RenameKeysNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let mut out = Vec::new();
        for (i, it) in port(ctx, 0).iter().enumerate() {
            let mut json = it.json.clone();
            for k in 0..raw_array(ctx, "keys.key").len() {
                let cur = param_str(ctx, &format!("keys.key.{k}.currentKey"), i, "")?;
                let new = param_str(ctx, &format!("keys.key.{k}.newKey"), i, "")?;
                if cur.is_empty() || new.is_empty() {
                    continue;
                }
                if let Some(v) = json.remove(&cur) {
                    json.insert(new, v);
                }
            }
            for r in 0..raw_array(ctx, "additionalOptions.regexReplacement.replacements").len() {
                let base = format!("additionalOptions.regexReplacement.replacements.{r}");
                let pat = param_str(ctx, &format!("{base}.searchRegex"), i, "")?;
                if pat.is_empty() {
                    continue;
                }
                let ci = param_bool(ctx, &format!("{base}.options.caseInsensitive"), i, false)?;
                let depth = param_f64(ctx, &format!("{base}.options.depth"), i, -1.0)? as i64;
                let re = regex::RegexBuilder::new(&pat)
                    .case_insensitive(ci)
                    .build()
                    .map_err(|e| node_err(ctx, format!("Invalid regular expression: {e}")))?;
                let with = param_str(ctx, &format!("{base}.replaceRegex"), i, "")?;
                json = rename_deep(&json, &re, &with, depth);
            }
            let mut o = it.clone();
            o.json = json;
            o.paired_item = paired(i);
            out.push(o);
        }
        Ok(vec![out])
    }
}

// ------------------------------------------------------------- Compare Datasets

simple_node!(CompareDatasetsNode);

impl CompareDatasetsNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Compare Datasets",
            "name": "n8n-nodes-base.compareDatasets",
            "version": [1, 2, 2.1, 2.2, 2.3],
            "defaultVersion": 2.3,
            "description": "Compare two inputs for changes",
            "group": ["transform"],
            "defaults": {"name": "Compare Datasets"},
            "inputs": ["main", "main"],
            "outputs": ["main", "main", "main", "main"],
            "outputNames": ["In A only", "Same", "Different", "In B only"],
            "properties": [
                {"displayName": "Fields to Match", "name": "mergeByFields", "type": "fixedCollection", "default": {"values": [{"field1": "", "field2": ""}]},
                 "typeOptions": {"multipleValues": true},
                 "options": [{"displayName": "Values", "name": "values", "values": [
                    {"displayName": "Input A Field", "name": "field1", "type": "string", "default": ""},
                    {"displayName": "Input B Field", "name": "field2", "type": "string", "default": ""}]}]},
                {"displayName": "When There Are Differences", "name": "resolve", "type": "options", "default": "includeBoth",
                 "options": [{"name": "Use Input A Version", "value": "preferInput1"}, {"name": "Use Input B Version", "value": "preferInput2"},
                             {"name": "Use a Mix of Versions", "value": "mix"}, {"name": "Include Both Versions", "value": "includeBoth"}]},
                {"displayName": "Fuzzy Compare", "name": "fuzzyCompare", "type": "boolean", "default": false},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {}, "placeholder": "Add option", "options": [
                    {"displayName": "Skip Fields", "name": "skipFields", "type": "string", "default": ""},
                    {"displayName": "Disable Dot Notation", "name": "disableDotNotation", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for CompareDatasetsNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let (a, b) = (port(ctx, 0), port(ctx, 1));
        let dot = !param_bool(ctx, "options.disableDotNotation", 0, false)?;
        let skip = param_list(ctx, "options.skipFields", 0)?;
        let resolve = param_str(ctx, "resolve", 0, "includeBoth")?;
        let mut pairs: Vec<(String, String)> = Vec::new();
        for k in 0..raw_array(ctx, "mergeByFields.values").len() {
            let f1 = param_str(ctx, &format!("mergeByFields.values.{k}.field1"), 0, "")?;
            let f2 = param_str(ctx, &format!("mergeByFields.values.{k}.field2"), 0, "")?;
            if !f1.is_empty() {
                let f2 = if f2.is_empty() { f1.clone() } else { f2 };
                pairs.push((f1, f2));
            }
        }
        if pairs.is_empty() {
            return Err(node_err(ctx, "Specify at least one field to match on"));
        }
        let key_of = |it: &Item, side_a: bool| -> String {
            let k: Vec<Value> = pairs
                .iter()
                .map(|(f1, f2)| {
                    get_path(&it.json, if side_a { f1 } else { f2 }, dot)
                        .cloned()
                        .unwrap_or(Value::Null)
                })
                .collect();
            canonical(&Value::Array(k))
        };
        let mut b_index: IndexMap<String, Vec<usize>> = IndexMap::new();
        for (j, it) in b.iter().enumerate() {
            b_index.entry(key_of(it, false)).or_default().push(j);
        }
        let mut used_b = vec![false; b.len()];
        let (mut only_a, mut same, mut different) = (Vec::new(), Vec::new(), Vec::new());
        for it in a {
            let hit = b_index.get_mut(&key_of(it, true)).and_then(|v| {
                if v.is_empty() {
                    None
                } else {
                    Some(v.remove(0))
                }
            });
            let Some(j) = hit else {
                only_a.push(it.clone());
                continue;
            };
            used_b[j] = true;
            let strip = |m: &Map<String, Value>| -> Map<String, Value> {
                m.iter()
                    .filter(|(k, _)| !skip.contains(k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            };
            let (ja, jb) = (strip(&it.json), strip(&b[j].json));
            if ja == jb {
                same.push(it.clone());
                continue;
            }
            let mut keys = Map::new();
            for (f1, _) in &pairs {
                keys.insert(
                    f1.clone(),
                    get_path(&it.json, f1, dot).cloned().unwrap_or(Value::Null),
                );
            }
            let mut same_f = Map::new();
            let mut diff_f = Map::new();
            let all: std::collections::BTreeSet<&String> = ja.keys().chain(jb.keys()).collect();
            for k in all {
                match (ja.get(k), jb.get(k)) {
                    (Some(x), Some(y)) if x == y => {
                        same_f.insert(k.clone(), x.clone());
                    }
                    (x, y) => {
                        let (x, y) = (
                            x.cloned().unwrap_or(Value::Null),
                            y.cloned().unwrap_or(Value::Null),
                        );
                        let v = match resolve.as_str() {
                            "preferInput1" => x,
                            "preferInput2" => y,
                            "mix" => y,
                            _ => json!({"inputA": x, "inputB": y}),
                        };
                        diff_f.insert(k.clone(), v);
                    }
                }
            }
            let mut m = Map::new();
            m.insert("keys".into(), Value::Object(keys));
            m.insert("same".into(), Value::Object(same_f));
            m.insert("different".into(), Value::Object(diff_f));
            different.push(Item::new(m));
        }
        let only_b: Vec<Item> = b
            .iter()
            .enumerate()
            .filter(|(j, _)| !used_b[*j])
            .map(|(_, it)| it.clone())
            .collect();
        Ok(vec![only_a, same, different, only_b])
    }
}

// ----------------------------------------------------------- Convert to File

simple_node!(ConvertToFileNode);

impl ConvertToFileNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Convert to File",
            "name": "n8n-nodes-base.convertToFile",
            "version": [1, 1.1],
            "description": "Convert JSON data to binary data",
            "group": ["input"],
            "defaults": {"name": "Convert to File"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Operation", "name": "operation", "type": "options", "default": "csv",
                 "options": [{"name": "Convert to CSV", "value": "csv"}, {"name": "Convert to JSON", "value": "toJson"},
                             {"name": "Convert to Text File", "value": "toText"}, {"name": "Move Base64 String to File", "value": "toBinary"}]},
                {"displayName": "Source Property", "name": "sourceProperty", "type": "string", "default": "",
                 "displayOptions": {"show": {"operation": ["toText", "toBinary"]}}},
                {"displayName": "Put Output File in Field", "name": "binaryPropertyName", "type": "string", "default": "data"},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {}, "placeholder": "Add option", "options": [
                    {"displayName": "File Name", "name": "fileName", "type": "string", "default": ""},
                    {"displayName": "Mode", "name": "mode", "type": "options", "default": "once",
                     "options": [{"name": "All Items to One File", "value": "once"}, {"name": "Each Item to Separate File", "value": "each"}]},
                    {"displayName": "Delimiter", "name": "delimiter", "type": "string", "default": ","},
                    {"displayName": "Header Row", "name": "headerRow", "type": "boolean", "default": true}]}
            ]
        })))
    }
}

fn csv_cell(v: &Value, delim: char) -> String {
    let s = match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if s.contains(delim) || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

fn make_binary(bytes: &[u8], mime: &str, name: &str, ext: &str) -> BinaryData {
    BinaryData {
        data: B64.encode(bytes),
        mime_type: mime.to_owned(),
        file_type: Some(mime.split('/').next().unwrap_or("application").to_owned()),
        file_name: Some(name.to_owned()),
        file_extension: Some(ext.to_owned()),
        file_size: Some(Value::String(human_size(bytes.len()))),
        ..BinaryData::default()
    }
}

fn human_size(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} kB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1048576.0)
    }
}

fn with_binary(prop: &str, b: BinaryData) -> Item {
    let mut it = Item::default();
    let mut m = IndexMap::new();
    m.insert(prop.to_owned(), b);
    it.binary = Some(m);
    it
}

#[async_trait]
impl FlowNode for ConvertToFileNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let items = port(ctx, 0);
        let op = param_str(ctx, "operation", 0, "csv")?;
        let prop = param_str(ctx, "binaryPropertyName", 0, "data")?;
        let each = param_str(ctx, "options.mode", 0, "once")? == "each";
        let name = param_str(ctx, "options.fileName", 0, "")?;
        let delim = param_str(ctx, "options.delimiter", 0, ",")?
            .chars()
            .next()
            .unwrap_or(',');
        let header = param_bool(ctx, "options.headerRow", 0, true)?;
        let mut out = Vec::new();
        match op.as_str() {
            "toBinary" | "toText" => {
                let src = param_str(ctx, "sourceProperty", 0, "")?;
                for (i, it) in items.iter().enumerate() {
                    let v = get_path(&it.json, &src, true).ok_or_else(|| {
                        node_err(ctx, format!("No property named '{src}' exists in item {i}"))
                    })?;
                    let text = to_display(v);
                    let bytes = if op == "toBinary" {
                        B64.decode(text.trim()).map_err(|e| {
                            node_err(ctx, format!("'{src}' is not valid base64: {e}"))
                        })?
                    } else {
                        text.into_bytes()
                    };
                    let (mime, ext) = if op == "toText" {
                        ("text/plain", "txt")
                    } else {
                        ("application/octet-stream", "bin")
                    };
                    let fname = if name.is_empty() {
                        format!("file.{ext}")
                    } else {
                        name.clone()
                    };
                    let mut o = with_binary(&prop, make_binary(&bytes, mime, &fname, ext));
                    o.paired_item = paired(i);
                    out.push(o);
                }
            }
            "csv" | "toJson" => {
                let groups: Vec<(Vec<&Item>, usize)> = if each {
                    items
                        .iter()
                        .enumerate()
                        .map(|(i, it)| (vec![it], i))
                        .collect()
                } else if items.is_empty() {
                    Vec::new()
                } else {
                    vec![(items.iter().collect(), 0)]
                };
                for (g, idx) in groups {
                    let (bytes, mime, ext) = if op == "csv" {
                        let mut cols: Vec<String> = Vec::new();
                        for it in &g {
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
                        for it in &g {
                            lines.push(
                                cols.iter()
                                    .map(|c| {
                                        csv_cell(it.json.get(c).unwrap_or(&Value::Null), delim)
                                    })
                                    .collect::<Vec<_>>()
                                    .join(&d),
                            );
                        }
                        (lines.join("\n").into_bytes(), "text/csv", "csv")
                    } else {
                        let v: Vec<Value> =
                            g.iter().map(|it| Value::Object(it.json.clone())).collect();
                        let body = if each {
                            v.into_iter().next().unwrap_or(Value::Null)
                        } else {
                            Value::Array(v)
                        };
                        (
                            serde_json::to_vec_pretty(&body).unwrap_or_default(),
                            "application/json",
                            "json",
                        )
                    };
                    let fname = if name.is_empty() {
                        format!("file.{ext}")
                    } else {
                        name.clone()
                    };
                    let mut o = with_binary(&prop, make_binary(&bytes, mime, &fname, ext));
                    o.paired_item = paired(idx);
                    out.push(o);
                }
            }
            other => {
                return Err(node_err(
                    ctx,
                    format!("Operation '{other}' is not supported"),
                ));
            }
        }
        Ok(vec![out])
    }
}

// --------------------------------------------------------- Extract from File

simple_node!(ExtractFromFileNode);

impl ExtractFromFileNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Extract from File",
            "name": "n8n-nodes-base.extractFromFile",
            "version": [1, 1.1],
            "description": "Convert binary data to JSON",
            "group": ["input"],
            "defaults": {"name": "Extract from File"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Operation", "name": "operation", "type": "options", "default": "csv",
                 "options": [{"name": "Extract From CSV", "value": "csv"}, {"name": "Extract From JSON", "value": "fromJson"},
                             {"name": "Extract From Text File", "value": "text"}, {"name": "Move File to Base64 String", "value": "binaryToPropery"}]},
                {"displayName": "Input Binary Field", "name": "binaryPropertyName", "type": "string", "default": "data"},
                {"displayName": "Destination Output Field", "name": "destinationKey", "type": "string", "default": "data",
                 "displayOptions": {"show": {"operation": ["fromJson", "text", "binaryToPropery"]}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {}, "placeholder": "Add option", "options": [
                    {"displayName": "Delimiter", "name": "delimiter", "type": "string", "default": ","},
                    {"displayName": "Header Row", "name": "headerRow", "type": "boolean", "default": true},
                    {"displayName": "Skip Empty Lines", "name": "skipEmptyLines", "type": "boolean", "default": true}]}
            ]
        })))
    }
}

/// RFC 4180 records (quotes, escaped quotes, embedded newlines).
pub(crate) fn parse_csv(text: &str, delim: char) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut cell) = (Vec::new(), Vec::new(), String::new());
    let (mut quoted, mut chars) = (
        false,
        text.trim_start_matches('\u{feff}').chars().peekable(),
    );
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (c, false) if c == delim => row.push(std::mem::take(&mut cell)),
            ('\r', false) => {}
            ('\n', false) => {
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
            }
            (c, _) => cell.push(c),
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows
}

fn csv_value(s: &str) -> Value {
    if s.is_empty() {
        return Value::String(String::new());
    }
    Value::String(s.to_owned())
}

#[async_trait]
impl FlowNode for ExtractFromFileNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let op = param_str(ctx, "operation", 0, "csv")?;
        let mut out = Vec::new();
        for (i, it) in port(ctx, 0).iter().enumerate() {
            let prop = param_str(ctx, "binaryPropertyName", i, "data")?;
            let bin = it
                .binary
                .as_ref()
                .and_then(|b| b.get(&prop))
                .ok_or_else(|| node_err(ctx, format!("Item {i} has no binary field '{prop}'")))?;
            if bin.data.is_empty() {
                return Err(node_err(
                    ctx,
                    format!(
                        "Binary field '{prop}' is stored outside the item and cannot be read here"
                    ),
                ));
            }
            let bytes = B64
                .decode(bin.data.as_bytes())
                .map_err(|e| node_err(ctx, format!("Binary data is not valid base64: {e}")))?;
            let dest = param_str(ctx, "destinationKey", i, "data")?;
            let text = || String::from_utf8_lossy(&bytes).into_owned();
            let mk = |json: Map<String, Value>| {
                let mut o = Item::new(json);
                o.paired_item = paired(i);
                o
            };
            match op.as_str() {
                "csv" => {
                    let delim = param_str(ctx, "options.delimiter", i, ",")?
                        .chars()
                        .next()
                        .unwrap_or(',');
                    let header = param_bool(ctx, "options.headerRow", i, true)?;
                    let skip_empty = param_bool(ctx, "options.skipEmptyLines", i, true)?;
                    let mut rows = parse_csv(&text(), delim);
                    if skip_empty {
                        rows.retain(|r| !(r.len() == 1 && r[0].is_empty()));
                    }
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
                            m.insert(h.clone(), csv_value(r.get(k).map_or("", String::as_str)));
                        }
                        out.push(mk(m));
                    }
                }
                "fromJson" => {
                    let v: Value = serde_json::from_slice(&bytes)
                        .map_err(|e| node_err(ctx, format!("Invalid JSON file: {e}")))?;
                    let mut m = Map::new();
                    m.insert(dest, v);
                    out.push(mk(m));
                }
                "text" => {
                    let mut m = Map::new();
                    m.insert(dest, Value::String(text()));
                    out.push(mk(m));
                }
                "binaryToPropery" => {
                    let mut m = Map::new();
                    m.insert(dest, Value::String(bin.data.clone()));
                    out.push(mk(m));
                }
                other => {
                    return Err(node_err(
                        ctx,
                        format!("Operation '{other}' is not supported"),
                    ));
                }
            }
        }
        Ok(vec![out])
    }
}

// ------------------------------------------------------- Function (legacy code)

/// `function` (all items, `items` in scope) and `functionItem` (`item` in scope, return it).
/// Both predate the Code node; old exports still use them.
#[derive(Debug)]
pub struct LegacyFunctionNode {
    desc: NodeTypeDescription,
    per_item: bool,
}

impl LegacyFunctionNode {
    pub fn new(per_item: bool) -> Self {
        let (name, display, prop, def) = if per_item {
            (
                "functionItem",
                "Function Item",
                "functionCode",
                "// Code here will run once per input item\nreturn item;",
            )
        } else {
            (
                "function",
                "Function",
                "functionCode",
                "// Code here will run only once, no matter how many input items there are\nreturn items;",
            )
        };
        Self {
            desc: describe(json!({
                "displayName": display,
                "name": format!("n8n-nodes-base.{name}"),
                "version": [1],
                "description": "Run custom function code which gets executed once per item",
                "group": ["transform"],
                "defaults": {"name": display, "color": "#FF9922"},
                "inputs": ["main"],
                "outputs": ["main"],
                "properties": [{"displayName": "JavaScript Code", "name": prop, "type": "string", "default": def,
                                "typeOptions": {"editor": "codeNodeEditor", "rows": 10}}]
            })),
            per_item,
        }
    }
}

#[async_trait]
impl FlowNode for LegacyFunctionNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let code = match raw_param(ctx, "functionCode") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let input = port(ctx, 0);
        let evaluator = ctx.evaluator().map_err(|e| node_err(ctx, e.to_string()))?;
        if self.per_item {
            if input.is_empty() {
                return Ok(vec![Vec::new()]);
            }
            let src = format!(
                "const item = $input.item.json;\nconst __r = (function () {{\n{code}\n}})();\nif (__r === null || __r === undefined) return null;\nreturn {{ json: __r }};"
            );
            let out = evaluator
                .run_script(&src, ScriptMode::EachItem, &ctx.expr_scope(0))
                .map_err(|e| node_err(ctx, e.to_string()))?;
            let items = match out.value {
                Value::Array(v) => v
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| !out.undefined.contains(i))
                    .filter_map(|(_, v)| {
                        v.get("json")
                            .and_then(Value::as_object)
                            .map(|o| Item::new(o.clone()))
                    })
                    .collect(),
                _ => Vec::new(),
            };
            return Ok(vec![items]);
        }
        let src = format!("const items = $input.all();\nconst item = items[0];\n{code}");
        let out = evaluator
            .run_script(&src, ScriptMode::AllItems, &ctx.expr_scope(0))
            .map_err(|e| node_err(ctx, e.to_string()))?;
        let items = match out.value {
            Value::Array(v) => v
                .into_iter()
                .map(|v| match v.get("json").and_then(Value::as_object) {
                    Some(o) => Item::new(o.clone()),
                    None => Item::new(v.as_object().cloned().unwrap_or_default()),
                })
                .collect(),
            _ => return Err(node_err(ctx, "Function code must return an array of items")),
        };
        Ok(vec![items])
    }
}
