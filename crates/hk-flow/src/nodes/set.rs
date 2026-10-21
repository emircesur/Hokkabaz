//! Edit Fields (Set) v3.x: `manual` (assignments) and `raw`/`json` (JSON output) modes.

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use super::util::*;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::Item;
use crate::node::{FlowNode, NodeContext, NodeOutput};

#[derive(Debug)]
pub struct SetNode(NodeTypeDescription);

impl Default for SetNode {
    fn default() -> Self {
        Self::new()
    }
}

impl SetNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Edit Fields (Set)",
            "name": "n8n-nodes-base.set",
            "version": [3, 3.1, 3.2, 3.3, 3.4],
            "defaultVersion": 3.4,
            "description": "Modify, add, or remove item fields",
            "subtitle": "={{$parameter[\"mode\"] === \"raw\" ? \"JSON\" : \"Manual\"}}",
            "group": ["input"],
            "defaults": {"name": "Edit Fields", "color": "#0000FF"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Mode", "name": "mode", "type": "options", "default": "manual",
                 "options": [
                    {"name": "Manual Mapping", "value": "manual"},
                    {"name": "JSON", "value": "raw"}]},
                {"displayName": "Fields to Set", "name": "assignments", "type": "assignmentCollection",
                 "default": {}, "displayOptions": {"show": {"mode": ["manual"]}}},
                {"displayName": "JSON", "name": "jsonOutput", "type": "json", "default": "{\n  \"my_field_1\": \"value\"\n}",
                 "displayOptions": {"show": {"mode": ["raw"]}}},
                {"displayName": "Include Other Input Fields", "name": "includeOtherFields", "type": "boolean", "default": false},
                {"displayName": "Input Fields to Include", "name": "include", "type": "options", "default": "all",
                 "options": [
                    {"name": "All", "value": "all"},
                    {"name": "Selected", "value": "selected"},
                    {"name": "All Except", "value": "except"}],
                 "displayOptions": {"show": {"includeOtherFields": [true]}}},
                {"displayName": "Fields to Include", "name": "includeFields", "type": "string", "default": "",
                 "displayOptions": {"show": {"include": ["selected"], "includeOtherFields": [true]}}},
                {"displayName": "Fields to Exclude", "name": "excludeFields", "type": "string", "default": "",
                 "displayOptions": {"show": {"include": ["except"], "includeOtherFields": [true]}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [
                    {"displayName": "Include Binary Data", "name": "includeBinary", "type": "boolean", "default": true},
                    {"displayName": "Ignore Type Conversion Errors", "name": "ignoreConversionErrors", "type": "boolean", "default": false},
                    {"displayName": "Support Dot Notation", "name": "dotNotation", "type": "boolean", "default": true}]}
            ]
        })))
    }
}

/// Convert an assigned value to the declared type.
fn coerce(ty: &str, v: Value, name: &str) -> Result<Value, String> {
    let bad = || format!("Value for '{name}' isn't a {ty}");
    Ok(match (ty, v) {
        (_, Value::Null) => Value::Null,
        ("string", Value::String(s)) => Value::String(s),
        ("string", Value::Array(a)) => Value::String(Value::Array(a).to_string()),
        ("string", Value::Object(o)) => Value::String(Value::Object(o).to_string()),
        ("string", other) => Value::String(to_display(&other)),
        ("number", Value::Number(n)) => Value::Number(n),
        ("number", Value::String(s)) => num(s.trim().parse::<f64>().map_err(|_| bad())?),
        ("boolean", Value::Bool(b)) => Value::Bool(b),
        ("boolean", Value::String(s)) => match s.trim().to_lowercase().as_str() {
            "true" | "1" => Value::Bool(true),
            "false" | "0" => Value::Bool(false),
            _ => return Err(bad()),
        },
        ("boolean", Value::Number(n)) => Value::Bool(n.as_f64().is_some_and(|f| f != 0.0)),
        ("array", Value::Array(a)) => Value::Array(a),
        ("array", Value::String(s)) => match serde_json::from_str::<Value>(&s) {
            Ok(v @ Value::Array(_)) => v,
            _ => return Err(bad()),
        },
        ("object", Value::Object(o)) => Value::Object(o),
        ("object", Value::String(s)) => match serde_json::from_str::<Value>(&s) {
            Ok(v @ Value::Object(_)) => v,
            _ => return Err(bad()),
        },
        ("number" | "boolean" | "array" | "object", _) => return Err(bad()),
        (_, other) => other,
    })
}

#[async_trait]
impl FlowNode for SetNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let items = port(ctx, 0);
        let mut out = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let mode = param_str(ctx, "mode", i, "manual")?;
            let dot = param_bool(ctx, "options.dotNotation", i, true)?;
            let ignore = param_bool(ctx, "options.ignoreConversionErrors", i, false)?;
            let include_other = param_bool(ctx, "includeOtherFields", i, false)?;

            let mut json = Map::new();
            if include_other {
                match param_str(ctx, "include", i, "all")?.as_str() {
                    "selected" => {
                        for f in param_list(ctx, "includeFields", i)? {
                            if let Some(v) = get_path(&item.json, &f, dot) {
                                set_path(&mut json, &f, v.clone(), dot);
                            }
                        }
                    }
                    "except" => {
                        json = item.json.clone();
                        for f in param_list(ctx, "excludeFields", i)? {
                            remove_path(&mut json, &f, dot);
                        }
                    }
                    _ => json = item.json.clone(),
                }
            }

            if mode == "raw" || mode == "json" {
                let v = param_or(ctx, "jsonOutput", i, Value::Null)?;
                let v = match v {
                    Value::String(s) => serde_json::from_str::<Value>(&s).map_err(|e| {
                        node_err(ctx, format!("Invalid JSON in 'JSON' field: {e} [item {i}]"))
                    })?,
                    other => other,
                };
                match v {
                    Value::Object(o) => {
                        for (k, v) in o {
                            json.insert(k, v);
                        }
                    }
                    _ => {
                        return Err(node_err(
                            ctx,
                            format!("The 'JSON' output isn't a valid object [item {i}]"),
                        ));
                    }
                }
            } else {
                for a in raw_array(ctx, "assignments.assignments") {
                    let name = match ctx.eval_param(a.get("name").unwrap_or(&Value::Null), i)? {
                        Value::String(s) => s,
                        other => to_display(&other),
                    };
                    if name.is_empty() {
                        continue;
                    }
                    let ty = a.get("type").and_then(Value::as_str).unwrap_or("string");
                    let value = ctx.eval_param(a.get("value").unwrap_or(&Value::Null), i)?;
                    let value = match coerce(ty, value.clone(), &name) {
                        Ok(v) => v,
                        Err(_) if ignore => value,
                        Err(m) => return Err(node_err(ctx, format!("{m} [item {i}]"))),
                    };
                    set_path(&mut json, &name, value, dot);
                }
            }

            let mut o = Item::new(json);
            o.paired_item = paired(i);
            let keep_bin = include_other
                && param_bool(ctx, "options.includeBinary", i, true)?
                && !param_bool(ctx, "options.stripBinary", i, false)?;
            if keep_bin {
                o.binary = item.binary.clone();
            }
            out.push(o);
        }
        Ok(vec![out])
    }
}
