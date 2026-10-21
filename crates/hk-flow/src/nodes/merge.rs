//! Merge v3: append, combine (by fields / position / all), choose branch.
//!
//! SQL mode (`combineBySql`) is not implemented: it needs an in-memory SQL engine.

use std::collections::HashSet;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use super::util::*;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::{Item, PairedItem, PairedSource};
use crate::node::{FlowNode, NodeContext, NodeOutput};

#[derive(Debug)]
pub struct MergeNode(NodeTypeDescription);

impl Default for MergeNode {
    fn default() -> Self {
        Self::new()
    }
}

impl MergeNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Merge",
            "name": "n8n-nodes-base.merge",
            "version": [3, 3.1, 3.2],
            "defaultVersion": 3.2,
            "description": "Merges data of multiple streams once data from both is available",
            "group": ["transform"],
            "defaults": {"name": "Merge", "color": "#00bbcc"},
            "inputs": "={{ Array(($parameter.numberInputs || 2)).fill('main') }}",
            "outputs": ["main"],
            "properties": [
                {"displayName": "Mode", "name": "mode", "type": "options", "default": "append",
                 "options": [
                    {"name": "Append", "value": "append"},
                    {"name": "Combine", "value": "combine"},
                    {"name": "SQL Query", "value": "combineBySql"},
                    {"name": "Choose Branch", "value": "chooseBranch"}]},
                {"displayName": "Combine By", "name": "combineBy", "type": "options", "default": "combineByFields",
                 "displayOptions": {"show": {"mode": ["combine"]}},
                 "options": [
                    {"name": "Matching Fields", "value": "combineByFields"},
                    {"name": "Position", "value": "combineByPosition"},
                    {"name": "All Possible Combinations", "value": "combineAll"}]},
                {"displayName": "Fields to Match", "name": "fieldsToMatchString", "type": "string", "default": "",
                 "displayOptions": {"show": {"mode": ["combine"], "combineBy": ["combineByFields"]}}},
                {"displayName": "Output Type", "name": "joinMode", "type": "options", "default": "keepMatches",
                 "displayOptions": {"show": {"mode": ["combine"], "combineBy": ["combineByFields"]}},
                 "options": [
                    {"name": "Keep Matches", "value": "keepMatches"},
                    {"name": "Keep Non-Matches", "value": "keepNonMatches"},
                    {"name": "Enrich Input 1", "value": "enrichInput1"},
                    {"name": "Enrich Input 2", "value": "enrichInput2"},
                    {"name": "Keep Everything", "value": "keepEverything"}]},
                {"displayName": "Number of Inputs", "name": "numberInputs", "type": "options", "default": 2,
                 "displayOptions": {"show": {"mode": ["append", "combineBySql", "chooseBranch"]}}},
                {"displayName": "Output", "name": "output", "type": "options", "default": "input1",
                 "displayOptions": {"show": {"mode": ["chooseBranch"]}},
                 "options": [
                    {"name": "Input 1 Data", "value": "input1"},
                    {"name": "Input 2 Data", "value": "input2"},
                    {"name": "A Single, Empty Item", "value": "empty"}]},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option", "options": [
                    {"displayName": "Clash Handling", "name": "clashHandling", "type": "fixedCollection", "default": {}},
                    {"displayName": "Include Unpaired Items", "name": "includeUnpaired", "type": "boolean", "default": false},
                    {"displayName": "Multiple Matches", "name": "multipleMatches", "type": "options", "default": "all"},
                    {"displayName": "Output Data From", "name": "outputDataFrom", "type": "options", "default": "both"},
                    {"displayName": "Disable Dot Notation", "name": "disableDotNotation", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clash {
    PreferInput1,
    PreferInput2,
    AddSuffix,
}

#[derive(Debug, Clone, Copy)]
struct MergeOpts {
    clash: Clash,
    deep: bool,
    override_empty: bool,
}

impl MergeOpts {
    fn read(ctx: &NodeContext) -> Result<Self, FlowError> {
        let base = "options.clashHandling.values";
        let clash = match param_str(ctx, &format!("{base}.resolveClash"), 0, "preferInput2")?
            .as_str()
        {
            "preferInput1" => Clash::PreferInput1,
            "addSuffix" => Clash::AddSuffix,
            _ => Clash::PreferInput2,
        };
        Ok(Self {
            clash,
            deep: param_str(ctx, &format!("{base}.mergeMode"), 0, "deepMerge")? != "shallowMerge",
            override_empty: param_bool(ctx, &format!("{base}.overrideEmpty"), 0, false)?,
        })
    }
}

fn is_empty_value(v: &Value) -> bool {
    matches!(v, Value::Null) || matches!(v, Value::String(s) if s.is_empty())
}

/// Merge `b` over/under `a` according to the clash policy; key order: all of `a`, then new keys of `b`.
fn merge_json(a: &Map<String, Value>, b: &Map<String, Value>, o: MergeOpts) -> Map<String, Value> {
    let mut out = Map::new();
    if o.clash == Clash::AddSuffix {
        for (k, v) in a {
            let key = if b.contains_key(k) { format!("{k}_1") } else { k.clone() };
            out.insert(key, v.clone());
        }
        for (k, v) in b {
            let key = if a.contains_key(k) { format!("{k}_2") } else { k.clone() };
            out.insert(key, v.clone());
        }
        return out;
    }
    let prefer2 = o.clash == Clash::PreferInput2;
    out.clone_from(a);
    for (k, bv) in b {
        match out.get_mut(k) {
            None => {
                out.insert(k.clone(), bv.clone());
            }
            Some(av) => {
                if o.deep
                    && let (Value::Object(ao), Value::Object(bo)) = (&*av, bv)
                {
                    *av = Value::Object(merge_json(ao, bo, o));
                    continue;
                }
                let (pref, other) = if prefer2 { (bv, &*av) } else { (&*av, bv) };
                let chosen = if o.override_empty && is_empty_value(pref) && !is_empty_value(other)
                {
                    other.clone()
                } else {
                    pref.clone()
                };
                *av = chosen;
            }
        }
    }
    out
}

fn merge_items(a: &Item, b: &Item, ai: usize, bi: usize, o: MergeOpts) -> Item {
    let mut item = Item::new(merge_json(&a.json, &b.json, o));
    if a.binary.is_some() || b.binary.is_some() {
        let mut bin = a.binary.clone().unwrap_or_default();
        for (k, v) in b.binary.iter().flatten() {
            bin.insert(k.clone(), v.clone());
        }
        item.binary = Some(bin);
    }
    item.paired_item = Some(PairedItem::Many(vec![
        PairedSource { item: ai, input: None, extra: Map::new() },
        PairedSource { item: bi, input: Some(1), extra: Map::new() },
    ]));
    item
}

fn from_port(item: &Item, idx: usize, port: usize) -> Item {
    let mut o = item.clone();
    o.paired_item = Some(PairedItem::Source(PairedSource {
        item: idx,
        input: (port > 0).then_some(port),
        extra: Map::new(),
    }));
    o
}

#[async_trait]
impl FlowNode for MergeNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let mode = param_str(ctx, "mode", 0, "append")?;
        let (a, b) = (port(ctx, 0), port(ctx, 1));
        match mode.as_str() {
            "append" => {
                let mut out = Vec::new();
                for (p, items) in ctx.input.iter().enumerate() {
                    out.extend(items.iter().enumerate().map(|(i, it)| from_port(it, i, p)));
                }
                Ok(vec![out])
            }
            "chooseBranch" => {
                let which = param_str(ctx, "output", 0, "input1")?;
                let out = match which.as_str() {
                    "empty" => vec![Item::default()],
                    "input2" => b.iter().enumerate().map(|(i, it)| from_port(it, i, 1)).collect(),
                    _ => a.iter().enumerate().map(|(i, it)| from_port(it, i, 0)).collect(),
                };
                Ok(vec![out])
            }
            "combineBySql" => Err(node_err(ctx, "Merge mode 'SQL Query' is not supported")),
            "combine" => {
                let by = param_str(ctx, "combineBy", 0, "combineByFields")?;
                let o = MergeOpts::read(ctx)?;
                match by.as_str() {
                    "combineByPosition" => {
                        let unpaired = param_bool(ctx, "options.includeUnpaired", 0, false)?;
                        let n = if unpaired { a.len().max(b.len()) } else { a.len().min(b.len()) };
                        let mut out = Vec::with_capacity(n);
                        for i in 0..n {
                            out.push(match (a.get(i), b.get(i)) {
                                (Some(x), Some(y)) => merge_items(x, y, i, i, o),
                                (Some(x), None) => from_port(x, i, 0),
                                (None, Some(y)) => from_port(y, i, 1),
                                (None, None) => continue,
                            });
                        }
                        Ok(vec![out])
                    }
                    "combineAll" => {
                        let mut out = Vec::with_capacity(a.len() * b.len());
                        for (i, x) in a.iter().enumerate() {
                            for (j, y) in b.iter().enumerate() {
                                out.push(merge_items(x, y, i, j, o));
                            }
                        }
                        Ok(vec![out])
                    }
                    "combineBySql" => Err(node_err(ctx, "Merge mode 'SQL Query' is not supported")),
                    _ => combine_by_fields(ctx, a, b, o).map(|o| vec![o]),
                }
            }
            other => Err(node_err(ctx, format!("Unknown Merge mode `{other}`"))),
        }
    }
}

fn match_fields(ctx: &NodeContext) -> Result<Vec<(String, String)>, FlowError> {
    let advanced = param_bool(ctx, "advanced", 0, false)?;
    if advanced || raw_param(ctx, "fieldsToMatchString").is_none() {
        let pairs: Vec<(String, String)> = raw_array(ctx, "mergeByFields.values")
            .iter()
            .filter_map(|v| {
                let f1 = v.get("field1")?.as_str()?.to_owned();
                let f2 = v.get("field2").and_then(Value::as_str).unwrap_or(&f1).to_owned();
                Some((f1, f2))
            })
            .collect();
        if !pairs.is_empty() {
            return Ok(pairs);
        }
    }
    Ok(param_list(ctx, "fieldsToMatchString", 0)?
        .into_iter()
        .map(|f| (f.clone(), f))
        .collect())
}

/// Join key of an item; `None` when any matching field is missing.
fn key_of(item: &Item, fields: &[&String], dot: bool) -> Option<String> {
    let mut parts = Vec::with_capacity(fields.len());
    for f in fields {
        let v = get_path(&item.json, f, dot)?;
        parts.push(canonical(v));
    }
    Some(parts.join("\u{1f}"))
}

fn combine_by_fields(
    ctx: &NodeContext,
    a: &[Item],
    b: &[Item],
    o: MergeOpts,
) -> Result<Vec<Item>, FlowError> {
    let pairs = match_fields(ctx)?;
    if pairs.is_empty() {
        return Err(node_err(ctx, "You need to define at least one pair of fields to match"));
    }
    let dot = !param_bool(ctx, "options.disableDotNotation", 0, false)?;
    let join = param_str(ctx, "joinMode", 0, "keepMatches")?;
    let from = param_str(ctx, "options.outputDataFrom", 0, "both")?;
    let all = param_str(ctx, "options.multipleMatches", 0, "all")? != "first";
    let f1: Vec<&String> = pairs.iter().map(|p| &p.0).collect();
    let f2: Vec<&String> = pairs.iter().map(|p| &p.1).collect();
    let k1: Vec<Option<String>> = a.iter().map(|i| key_of(i, &f1, dot)).collect();
    let k2: Vec<Option<String>> = b.iter().map(|i| key_of(i, &f2, dot)).collect();

    // matches of an input-1 item in input 2 (indices, input order)
    let matches_of = |k: &Option<String>, other: &[Option<String>]| -> Vec<usize> {
        let Some(k) = k else { return Vec::new() };
        let mut m: Vec<usize> = other
            .iter()
            .enumerate()
            .filter(|(_, ok)| ok.as_ref() == Some(k))
            .map(|(i, _)| i)
            .collect();
        if !all {
            m.truncate(1);
        }
        m
    };

    let mut out = Vec::new();
    let mut used2: HashSet<usize> = HashSet::new();
    let mut unmatched1 = Vec::new();
    let mut matched = Vec::new();
    for (i, k) in k1.iter().enumerate() {
        let m = matches_of(k, &k2);
        if m.is_empty() {
            unmatched1.push(i);
            continue;
        }
        for j in m {
            used2.insert(j);
            matched.push((i, j));
        }
    }
    // with multipleMatches=first, input-2 items that were not the first match still count
    // as matched for the "unmatched" bookkeeping
    for (j, k) in k2.iter().enumerate() {
        if let Some(k) = k
            && k1.iter().any(|x| x.as_ref() == Some(k))
        {
            used2.insert(j);
        }
    }
    let unmatched2: Vec<usize> = (0..b.len()).filter(|j| !used2.contains(j)).collect();

    let merged = |i: usize, j: usize| -> Item {
        match from.as_str() {
            "input1" => from_port(&a[i], i, 0),
            "input2" => from_port(&b[j], j, 1),
            _ => merge_items(&a[i], &b[j], i, j, o),
        }
    };

    match join.as_str() {
        "keepMatches" => {
            out.extend(matched.iter().map(|&(i, j)| merged(i, j)));
        }
        "keepNonMatches" => {
            if from != "input2" {
                out.extend(unmatched1.iter().map(|&i| from_port(&a[i], i, 0)));
            }
            if from != "input1" {
                out.extend(unmatched2.iter().map(|&j| from_port(&b[j], j, 1)));
            }
        }
        "enrichInput1" => {
            for (i, x) in a.iter().enumerate() {
                let mut any = false;
                for &(mi, j) in matched.iter().filter(|(mi, _)| *mi == i) {
                    any = true;
                    out.push(merge_items(&a[mi], &b[j], mi, j, o));
                }
                if !any {
                    out.push(from_port(x, i, 0));
                }
            }
        }
        "enrichInput2" => {
            // output follows input 2; matches come from input 1
            for (j, y) in b.iter().enumerate() {
                let mut any = false;
                for &(i, mj) in matched.iter().filter(|(_, mj)| *mj == j) {
                    any = true;
                    out.push(merge_items(&a[i], &b[mj], i, mj, o));
                }
                if !any {
                    out.push(from_port(y, j, 1));
                }
            }
        }
        "keepEverything" => {
            out.extend(matched.iter().map(|&(i, j)| merge_items(&a[i], &b[j], i, j, o)));
            out.extend(unmatched1.iter().map(|&i| from_port(&a[i], i, 0)));
            out.extend(unmatched2.iter().map(|&j| from_port(&b[j], j, 1)));
        }
        other => return Err(node_err(ctx, format!("Unknown join mode `{other}`"))),
    }
    Ok(out)
}
