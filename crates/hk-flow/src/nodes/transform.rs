//! Data-shaping nodes: Loop Over Items (splitInBatches v3), Limit, Sort, Remove
//! Duplicates, Aggregate, Split Out.

use std::cmp::Ordering;
use std::collections::HashSet;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use super::util::*;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::{Item, PairedItem, PairedSource};
use crate::node::{FlowNode, NodeContext, NodeOutput};

/// A field list given as `"a, b"` or as a fixedCollection (`{fields: [{fieldName}]}`,
/// `{field: [...]}`); either shape appears across node versions.
fn field_list(ctx: &NodeContext, path: &str, i: usize) -> Result<Vec<String>, FlowError> {
    let v = param(ctx, path, i)?.unwrap_or(Value::Null);
    Ok(match v {
        Value::String(s) => split_list(&s),
        Value::Array(a) => a.iter().map(to_display).collect(),
        Value::Object(o) => o
            .values()
            .filter_map(Value::as_array)
            .flatten()
            .filter_map(|e| e.get("fieldName").or_else(|| e.get("name")))
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    })
}

// ------------------------------------------------------------ Loop Over Items

#[derive(Debug)]
pub struct SplitInBatchesNode(NodeTypeDescription);

impl Default for SplitInBatchesNode {
    fn default() -> Self {
        Self::new()
    }
}

impl SplitInBatchesNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Loop Over Items (Split in Batches)",
            "name": "n8n-nodes-base.splitInBatches",
            "version": [3],
            "description": "Split data into batches and iterate over each batch",
            "group": ["organization"],
            "defaults": {"name": "Loop Over Items", "color": "#007755"},
            "inputs": ["main"],
            "outputs": ["main", "main"],
            "outputNames": ["done", "loop"],
            "properties": [
                {"displayName": "Batch Size", "name": "batchSize", "type": "number", "default": 1},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [{"displayName": "Reset", "name": "reset", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

fn items_of(v: Option<&Value>) -> Vec<Item> {
    v.and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

#[async_trait]
impl FlowNode for SplitInBatchesNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    /// Output 0 `done` fires once, with everything that came back through the loop;
    /// output 1 `loop` emits the next batch. Queue state lives in the execution scratch.
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let name = &ctx.node.name;
        let size = param_usize(ctx, "batchSize", 0, 1)?.max(1);
        let reset = param_bool(ctx, "options.reset", 0, false)?;
        let state = if reset { None } else { ctx.scratch.get(name) };

        let (mut remaining, mut processed) = match state {
            None => (port(ctx, 0).to_vec(), Vec::new()),
            Some(s) => {
                let mut processed = items_of(s.get("processed"));
                processed.extend(port(ctx, 0).iter().cloned());
                (items_of(s.get("remaining")), processed)
            }
        };

        if remaining.is_empty() {
            ctx.scratch.clear(name);
            return Ok(vec![processed, Vec::new()]);
        }
        let take = size.min(remaining.len());
        let batch: Vec<Item> = remaining.drain(..take).collect();
        // keep `processed` out of the loop output: it is only for the done port
        let state = json!({
            "remaining": remaining,
            "processed": std::mem::take(&mut processed),
        });
        ctx.scratch.set(name, state);
        Ok(vec![Vec::new(), batch])
    }
}

// ---------------------------------------------------------------------- Limit

#[derive(Debug)]
pub struct LimitNode(NodeTypeDescription);

impl Default for LimitNode {
    fn default() -> Self {
        Self::new()
    }
}

impl LimitNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Limit",
            "name": "n8n-nodes-base.limit",
            "version": [1],
            "description": "Restrict the number of items",
            "group": ["transform"],
            "defaults": {"name": "Limit"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Max Items", "name": "maxItems", "type": "number", "default": 1},
                {"displayName": "Keep", "name": "keep", "type": "options", "default": "firstItems",
                 "options": [
                    {"name": "First Items", "value": "firstItems"},
                    {"name": "Last Items", "value": "lastItems"}]}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for LimitNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let items = port(ctx, 0);
        let max = param_usize(ctx, "maxItems", 0, 1)?;
        let keep_last = param_str(ctx, "keep", 0, "firstItems")? == "lastItems";
        let start = if keep_last { items.len().saturating_sub(max) } else { 0 };
        let end = if keep_last { items.len() } else { max.min(items.len()) };
        let out = items[start..end]
            .iter()
            .enumerate()
            .map(|(k, it)| {
                let mut o = it.clone();
                o.paired_item = paired(start + k);
                o
            })
            .collect();
        Ok(vec![out])
    }
}

// ----------------------------------------------------------------------- Sort

#[derive(Debug)]
pub struct SortNode(NodeTypeDescription);

impl Default for SortNode {
    fn default() -> Self {
        Self::new()
    }
}

impl SortNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Sort",
            "name": "n8n-nodes-base.sort",
            "version": [1],
            "description": "Change items order",
            "group": ["transform"],
            "defaults": {"name": "Sort"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Type", "name": "type", "type": "options", "default": "simple",
                 "options": [
                    {"name": "Simple", "value": "simple"},
                    {"name": "Random", "value": "random"},
                    {"name": "Code", "value": "code"}]},
                {"displayName": "Fields To Sort By", "name": "sortFieldsUi", "type": "fixedCollection",
                 "default": {}, "typeOptions": {"multipleValues": true},
                 "displayOptions": {"show": {"type": ["simple"]}},
                 "options": [{"name": "sortField", "displayName": "Sort Field", "values": [
                    {"displayName": "Field Name", "name": "fieldName", "type": "string", "default": ""},
                    {"displayName": "Order", "name": "order", "type": "options", "default": "ascending",
                     "options": [{"name": "Ascending", "value": "ascending"}, {"name": "Descending", "value": "descending"}]}]}]},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [{"displayName": "Disable Dot Notation", "name": "disableDotNotation", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

/// Case-insensitive first, raw text as tie-break (what a locale compare approximates).
fn cmp_sort(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x
            .to_lowercase()
            .cmp(&y.to_lowercase())
            .then_with(|| y.cmp(x)),
        _ => cmp_values(a, b),
    }
}

/// xorshift64*; a shuffle needs no cryptographic randomness.
fn shuffle<T>(v: &mut [T]) {
    let mut s = (super::date::now_millis() as u64) ^ 0x9E37_79B9_7F4A_7C15 ^ (v.len() as u64);
    s = s.max(1);
    for i in (1..v.len()).rev() {
        s ^= s >> 12;
        s ^= s << 25;
        s ^= s >> 27;
        let r = s.wrapping_mul(0x2545_F491_4F6C_DD1D);
        v.swap(i, (r % (i as u64 + 1)) as usize);
    }
}

#[async_trait]
impl FlowNode for SortNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let mut idx: Vec<usize> = (0..port(ctx, 0).len()).collect();
        let items = port(ctx, 0);
        match param_str(ctx, "type", 0, "simple")?.as_str() {
            "random" => shuffle(&mut idx),
            "code" => {
                return Err(node_err(ctx, "Sort type 'Code' is not supported"));
            }
            _ => {
                let dot = !param_bool(ctx, "options.disableDotNotation", 0, false)?;
                let mut keys: Vec<(String, bool)> = Vec::new();
                for f in raw_array(ctx, "sortFieldsUi.sortField") {
                    let name = f.get("fieldName").and_then(Value::as_str).unwrap_or("");
                    if name.is_empty() {
                        return Err(node_err(ctx, "No sorting specified. Please add a field to sort by"));
                    }
                    let desc = f.get("order").and_then(Value::as_str) == Some("descending");
                    keys.push((name.to_owned(), desc));
                }
                if keys.is_empty() {
                    return Err(node_err(ctx, "No sorting specified. Please add a field to sort by"));
                }
                let null = Value::Null;
                idx.sort_by(|&x, &y| {
                    for (f, desc) in &keys {
                        let a = get_path(&items[x].json, f, dot).unwrap_or(&null);
                        let b = get_path(&items[y].json, f, dot).unwrap_or(&null);
                        let o = cmp_sort(a, b);
                        let o = if *desc { o.reverse() } else { o };
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    Ordering::Equal
                });
            }
        }
        Ok(vec![
            idx.into_iter()
                .map(|i| {
                    let mut o = items[i].clone();
                    o.paired_item = paired(i);
                    o
                })
                .collect(),
        ])
    }
}

// ----------------------------------------------------------- Remove Duplicates

#[derive(Debug)]
pub struct RemoveDuplicatesNode(NodeTypeDescription);

impl Default for RemoveDuplicatesNode {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoveDuplicatesNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Remove Duplicates",
            "name": "n8n-nodes-base.removeDuplicates",
            "version": [1, 1.1, 2],
            "defaultVersion": 2,
            "description": "Delete items with matching field values",
            "group": ["transform"],
            "defaults": {"name": "Remove Duplicates"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Operation", "name": "operation", "type": "options", "default": "removeDuplicateInputItems",
                 "options": [
                    {"name": "Remove Items Repeated Within Current Input", "value": "removeDuplicateInputItems"},
                    {"name": "Remove Items Processed in Previous Executions", "value": "removeItemsSeenInPreviousExecutions"},
                    {"name": "Clear Deduplication History", "value": "clearDeduplicationHistory"}]},
                {"displayName": "Compare", "name": "compare", "type": "options", "default": "allFields",
                 "options": [
                    {"name": "All Fields", "value": "allFields"},
                    {"name": "All Fields Except", "value": "allFieldsExcept"},
                    {"name": "Selected Fields", "value": "selectedFields"}]},
                {"displayName": "Fields To Exclude", "name": "fieldsToExclude", "type": "string", "default": "",
                 "displayOptions": {"show": {"compare": ["allFieldsExcept"]}}},
                {"displayName": "Fields To Compare", "name": "fieldsToCompare", "type": "string", "default": "",
                 "displayOptions": {"show": {"compare": ["selectedFields"]}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [
                    {"displayName": "Disable Dot Notation", "name": "disableDotNotation", "type": "boolean", "default": false},
                    {"displayName": "Remove Other Fields", "name": "removeOtherFields", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for RemoveDuplicatesNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let op = param_str(ctx, "operation", 0, "removeDuplicateInputItems")?;
        if op != "removeDuplicateInputItems" {
            return Err(node_err(
                ctx,
                "Cross-execution deduplication needs persistent storage and is not supported yet",
            ));
        }
        let dot = !param_bool(ctx, "options.disableDotNotation", 0, false)?;
        let remove_other = param_bool(ctx, "options.removeOtherFields", 0, false)?;
        let compare = param_str(ctx, "compare", 0, "allFields")?;
        let selected = if compare == "selectedFields" {
            field_list(ctx, "fieldsToCompare", 0)?
        } else {
            Vec::new()
        };
        let excluded = if compare == "allFieldsExcept" {
            field_list(ctx, "fieldsToExclude", 0)?
        } else {
            Vec::new()
        };
        if compare == "selectedFields" && selected.is_empty() {
            return Err(node_err(ctx, "No fields specified. Please add a field to compare on"));
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for (i, item) in port(ctx, 0).iter().enumerate() {
            let basis: Map<String, Value> = match compare.as_str() {
                "selectedFields" => {
                    let mut m = Map::new();
                    for f in &selected {
                        if let Some(v) = get_path(&item.json, f, dot) {
                            set_path(&mut m, f, v.clone(), dot);
                        }
                    }
                    m
                }
                "allFieldsExcept" => {
                    let mut m = item.json.clone();
                    for f in &excluded {
                        remove_path(&mut m, f, dot);
                    }
                    m
                }
                _ => item.json.clone(),
            };
            if seen.insert(canonical(&Value::Object(basis.clone()))) {
                let mut o = item.clone();
                if remove_other && compare == "selectedFields" {
                    o.json = basis;
                }
                o.paired_item = paired(i);
                out.push(o);
            }
        }
        Ok(vec![out])
    }
}

// ------------------------------------------------------------------- Aggregate

#[derive(Debug)]
pub struct AggregateNode(NodeTypeDescription);

impl Default for AggregateNode {
    fn default() -> Self {
        Self::new()
    }
}

impl AggregateNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Aggregate",
            "name": "n8n-nodes-base.aggregate",
            "version": [1],
            "description": "Combine a field from many items into a list in a single item",
            "group": ["transform"],
            "defaults": {"name": "Aggregate"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Aggregate", "name": "aggregate", "type": "options", "default": "aggregateIndividualFields",
                 "options": [
                    {"name": "Individual Fields", "value": "aggregateIndividualFields"},
                    {"name": "All Item Data (Into a Single List)", "value": "aggregateAllItemData"}]},
                {"displayName": "Fields To Aggregate", "name": "fieldsToAggregate", "type": "fixedCollection",
                 "default": {"fieldToAggregate": [{"fieldToAggregate": "", "renameField": false}]},
                 "typeOptions": {"multipleValues": true},
                 "displayOptions": {"show": {"aggregate": ["aggregateIndividualFields"]}},
                 "options": [{"name": "fieldToAggregate", "displayName": "Field To Aggregate", "values": [
                    {"displayName": "Input Field Name", "name": "fieldToAggregate", "type": "string", "default": ""},
                    {"displayName": "Rename Field", "name": "renameField", "type": "boolean", "default": false},
                    {"displayName": "Output Field Name", "name": "outputFieldName", "type": "string", "default": ""}]}]},
                {"displayName": "Put Output in Field", "name": "destinationFieldName", "type": "string", "default": "data",
                 "displayOptions": {"show": {"aggregate": ["aggregateAllItemData"]}}},
                {"displayName": "Include", "name": "include", "type": "options", "default": "allFields",
                 "displayOptions": {"show": {"aggregate": ["aggregateAllItemData"]}},
                 "options": [
                    {"name": "All Fields", "value": "allFields"},
                    {"name": "Specified Fields", "value": "specifiedFields"},
                    {"name": "All Fields Except", "value": "allFieldsExcept"}]},
                {"displayName": "Fields To Exclude", "name": "fieldsToExclude", "type": "string", "default": ""},
                {"displayName": "Fields To Include", "name": "fieldsToInclude", "type": "string", "default": ""},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [
                    {"displayName": "Disable Dot Notation", "name": "disableDotNotation", "type": "boolean", "default": false},
                    {"displayName": "Merge Lists", "name": "mergeLists", "type": "boolean", "default": false},
                    {"displayName": "Include Binaries", "name": "includeBinaries", "type": "boolean", "default": false},
                    {"displayName": "Keep Missing And Null Values", "name": "keepMissing", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

fn all_sources(n: usize) -> Option<PairedItem> {
    Some(PairedItem::Many(
        (0..n)
            .map(|item| PairedSource { item, input: None, extra: Map::new() })
            .collect(),
    ))
}

#[async_trait]
impl FlowNode for AggregateNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let items = port(ctx, 0);
        if items.is_empty() {
            return Ok(vec![Vec::new()]);
        }
        let dot = !param_bool(ctx, "options.disableDotNotation", 0, false)?;
        let keep_missing = param_bool(ctx, "options.keepMissing", 0, false)?;
        let mut json = Map::new();

        if param_str(ctx, "aggregate", 0, "aggregateIndividualFields")? == "aggregateAllItemData" {
            let dest = param_str(ctx, "destinationFieldName", 0, "data")?;
            let include = param_str(ctx, "include", 0, "allFields")?;
            let fields = match include.as_str() {
                "specifiedFields" => field_list(ctx, "fieldsToInclude", 0)?,
                "allFieldsExcept" => field_list(ctx, "fieldsToExclude", 0)?,
                _ => Vec::new(),
            };
            let rows: Vec<Value> = items
                .iter()
                .map(|it| {
                    let row = match include.as_str() {
                        "specifiedFields" => {
                            let mut m = Map::new();
                            for f in &fields {
                                if let Some(v) = get_path(&it.json, f, dot) {
                                    set_path(&mut m, f, v.clone(), dot);
                                }
                            }
                            m
                        }
                        "allFieldsExcept" => {
                            let mut m = it.json.clone();
                            for f in &fields {
                                remove_path(&mut m, f, dot);
                            }
                            m
                        }
                        _ => it.json.clone(),
                    };
                    Value::Object(row)
                })
                .collect();
            json.insert(dest, Value::Array(rows));
        } else {
            let merge_lists = param_bool(ctx, "options.mergeLists", 0, false)?;
            let specs = raw_array(ctx, "fieldsToAggregate.fieldToAggregate");
            if specs.is_empty() {
                return Err(node_err(ctx, "No fields specified. Please add a field to aggregate"));
            }
            for spec in specs {
                let field = spec.get("fieldToAggregate").and_then(Value::as_str).unwrap_or("");
                if field.is_empty() {
                    return Err(node_err(ctx, "Field to aggregate is empty"));
                }
                let rename = spec.get("renameField").and_then(Value::as_bool).unwrap_or(false);
                let out_name = match spec.get("outputFieldName").and_then(Value::as_str) {
                    Some(n) if rename && !n.is_empty() => n,
                    _ => field,
                };
                let mut vals = Vec::new();
                for it in items {
                    match get_path(&it.json, field, dot) {
                        Some(Value::Array(a)) if merge_lists => vals.extend(a.iter().cloned()),
                        Some(v) => vals.push(v.clone()),
                        None if keep_missing => vals.push(Value::Null),
                        None => {}
                    }
                }
                set_path(&mut json, out_name, Value::Array(vals), dot);
            }
        }
        let mut o = Item::new(json);
        o.paired_item = all_sources(items.len());
        Ok(vec![vec![o]])
    }
}

// ------------------------------------------------------------------- Split Out

#[derive(Debug)]
pub struct SplitOutNode(NodeTypeDescription);

impl Default for SplitOutNode {
    fn default() -> Self {
        Self::new()
    }
}

impl SplitOutNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Split Out",
            "name": "n8n-nodes-base.splitOut",
            "version": [1],
            "description": "Turn a list inside item(s) into separate items",
            "group": ["transform"],
            "defaults": {"name": "Split Out"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Fields To Split Out", "name": "fieldToSplitOut", "type": "string", "default": ""},
                {"displayName": "Include", "name": "include", "type": "options", "default": "noOtherFields",
                 "options": [
                    {"name": "No Other Fields", "value": "noOtherFields"},
                    {"name": "All Other Fields", "value": "allOtherFields"},
                    {"name": "Selected Other Fields", "value": "selectedOtherFields"}]},
                {"displayName": "Fields To Include", "name": "fieldsToInclude", "type": "string", "default": "",
                 "displayOptions": {"show": {"include": ["selectedOtherFields"]}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [
                    {"displayName": "Disable Dot Notation", "name": "disableDotNotation", "type": "boolean", "default": false},
                    {"displayName": "Destination Field Name", "name": "destinationFieldName", "type": "string", "default": ""},
                    {"displayName": "Include Binary", "name": "includeBinary", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for SplitOutNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let mut out = Vec::new();
        for (i, item) in port(ctx, 0).iter().enumerate() {
            let fields = param_list(ctx, "fieldToSplitOut", i)?;
            if fields.is_empty() {
                return Err(node_err(ctx, "No field specified to split out"));
            }
            let dest = param_list(ctx, "options.destinationFieldName", i)?;
            let dot = !param_bool(ctx, "options.disableDotNotation", i, false)?;
            let include = param_str(ctx, "include", i, "noOtherFields")?;
            let keep_bin = param_bool(ctx, "options.includeBinary", i, false)?;

            let mut base = Map::new();
            match include.as_str() {
                "allOtherFields" => {
                    base = item.json.clone();
                    for f in &fields {
                        remove_path(&mut base, f, dot);
                    }
                }
                "selectedOtherFields" => {
                    for f in param_list(ctx, "fieldsToInclude", i)? {
                        if let Some(v) = get_path(&item.json, &f, dot) {
                            set_path(&mut base, &f, v.clone(), dot);
                        }
                    }
                }
                _ => {}
            }

            let mut columns: Vec<(String, bool, Vec<Value>)> = Vec::new();
            for (fi, f) in fields.iter().enumerate() {
                let Some(v) = get_path(&item.json, f, dot) else {
                    return Err(node_err(
                        ctx,
                        format!("The field '{f}' wasn't found in the input item [item {i}]"),
                    ));
                };
                let (name, renamed) = match dest.get(fi) {
                    Some(d) => (d.clone(), true),
                    None => (f.rsplit('.').next().unwrap_or(f).to_owned(), false),
                };
                let vals = match v {
                    Value::Array(a) => a.clone(),
                    other => vec![other.clone()],
                };
                columns.push((name, renamed, vals));
            }
            let n = columns.iter().map(|c| c.2.len()).max().unwrap_or(0);
            for k in 0..n {
                let mut json = base.clone();
                for (name, renamed, vals) in &columns {
                    let Some(v) = vals.get(k) else { continue };
                    match v {
                        // a list of objects becomes the items themselves unless renamed
                        Value::Object(o) if !renamed && columns.len() == 1 => {
                            for (ok, ov) in o {
                                json.insert(ok.clone(), ov.clone());
                            }
                        }
                        _ => {
                            json.insert(name.clone(), v.clone());
                        }
                    }
                }
                let mut o = Item::new(json);
                o.paired_item = paired(i);
                if keep_bin {
                    o.binary = item.binary.clone();
                }
                out.push(o);
            }
        }
        Ok(vec![out])
    }
}
