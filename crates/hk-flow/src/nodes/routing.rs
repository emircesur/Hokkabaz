//! If (v2), Switch (v3), Filter (v2), No Operation.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::conditions::{CondFlags, evaluate};
use super::util::*;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::Item;
use crate::node::{FlowNode, NodeContext, NodeOutput};

fn with_pair(item: &Item, i: usize) -> Item {
    let mut o = item.clone();
    o.paired_item = paired(i);
    o
}

fn conditions_param(ctx: &NodeContext) -> Value {
    raw_param(ctx, "conditions").cloned().unwrap_or(Value::Null)
}

// ------------------------------------------------------------------------- If

#[derive(Debug)]
pub struct IfNode(NodeTypeDescription);

impl Default for IfNode {
    fn default() -> Self {
        Self::new()
    }
}

impl IfNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "If",
            "name": "n8n-nodes-base.if",
            "version": [2, 2.1, 2.2],
            "defaultVersion": 2.2,
            "description": "Route items to different branches (true/false)",
            "group": ["transform"],
            "defaults": {"name": "If", "color": "#408000"},
            "inputs": ["main"],
            "outputs": ["main", "main"],
            "outputNames": ["true", "false"],
            "properties": [
                {"displayName": "Conditions", "name": "conditions", "type": "filter", "default": {},
                 "typeOptions": {"filter": {"caseSensitive": true, "typeValidation": "strict", "version": 2}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [
                    {"displayName": "Ignore Case", "name": "ignoreCase", "type": "boolean", "default": false},
                    {"displayName": "Less Strict Type Validation", "name": "looseTypeValidation", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for IfNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let cond = conditions_param(ctx);
        let (mut yes, mut no) = (Vec::new(), Vec::new());
        for (i, item) in port(ctx, 0).iter().enumerate() {
            let flags = CondFlags::from_options(ctx, i)?;
            if evaluate(ctx, &cond, i, flags)? {
                yes.push(with_pair(item, i));
            } else {
                no.push(with_pair(item, i));
            }
        }
        Ok(vec![yes, no])
    }
}

// ---------------------------------------------------------------------- Filter

#[derive(Debug)]
pub struct FilterNode(NodeTypeDescription);

impl Default for FilterNode {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Filter",
            "name": "n8n-nodes-base.filter",
            "version": [2, 2.1, 2.2],
            "defaultVersion": 2.2,
            "description": "Remove items matching a condition",
            "group": ["transform"],
            "defaults": {"name": "Filter", "color": "#229eff"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Conditions", "name": "conditions", "type": "filter", "default": {},
                 "typeOptions": {"filter": {"caseSensitive": true, "typeValidation": "strict", "version": 2}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [
                    {"displayName": "Ignore Case", "name": "ignoreCase", "type": "boolean", "default": false},
                    {"displayName": "Less Strict Type Validation", "name": "looseTypeValidation", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for FilterNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let cond = conditions_param(ctx);
        let mut kept = Vec::new();
        for (i, item) in port(ctx, 0).iter().enumerate() {
            let flags = CondFlags::from_options(ctx, i)?;
            if evaluate(ctx, &cond, i, flags)? {
                kept.push(with_pair(item, i));
            }
        }
        Ok(vec![kept])
    }
}

// ---------------------------------------------------------------------- Switch

#[derive(Debug)]
pub struct SwitchNode(NodeTypeDescription);

impl Default for SwitchNode {
    fn default() -> Self {
        Self::new()
    }
}

const SWITCH_OUTPUTS: &str = "={{ $parameter.mode === 'expression' ? Array.from({ length: $parameter.numberOutputs || 4 }, (_, i) => ({ type: 'main', displayName: i.toString() })) : [...($parameter.rules?.values ?? []).map((r, i) => ({ type: 'main', displayName: r.outputKey || i.toString() })), ...($parameter.options?.fallbackOutput === 'extra' ? [{ type: 'main', displayName: 'Fallback' }] : [])] }}";

impl SwitchNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Switch",
            "name": "n8n-nodes-base.switch",
            "version": [3, 3.1, 3.2],
            "defaultVersion": 3.2,
            "description": "Route items depending on defined expression or rules",
            "group": ["transform"],
            "defaults": {"name": "Switch", "color": "#506000"},
            "inputs": ["main"],
            "outputs": SWITCH_OUTPUTS,
            "properties": [
                {"displayName": "Mode", "name": "mode", "type": "options", "default": "rules",
                 "options": [
                    {"name": "Rules", "value": "rules"},
                    {"name": "Expression", "value": "expression"}]},
                {"displayName": "Routing Rules", "name": "rules", "type": "fixedCollection", "default": {"values": [{}]},
                 "typeOptions": {"multipleValues": true},
                 "displayOptions": {"show": {"mode": ["rules"]}},
                 "options": [{"name": "values", "displayName": "Values", "values": [
                    {"displayName": "Conditions", "name": "conditions", "type": "filter", "default": {}},
                    {"displayName": "Rename Output", "name": "renameOutput", "type": "boolean", "default": false},
                    {"displayName": "Output Name", "name": "outputKey", "type": "string", "default": ""}]}]},
                {"displayName": "Number of Outputs", "name": "numberOutputs", "type": "number", "default": 4,
                 "displayOptions": {"show": {"mode": ["expression"]}}},
                {"displayName": "Output Index", "name": "output", "type": "number", "default": "={{}}",
                 "displayOptions": {"show": {"mode": ["expression"]}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [
                    {"displayName": "Fallback Output", "name": "fallbackOutput", "type": "options", "default": "none"},
                    {"displayName": "Ignore Case", "name": "ignoreCase", "type": "boolean", "default": false},
                    {"displayName": "Less Strict Type Validation", "name": "looseTypeValidation", "type": "boolean", "default": false},
                    {"displayName": "Send data to all matching outputs", "name": "allMatchingOutputs", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for SwitchNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let items = port(ctx, 0);
        let expression_mode = param_str(ctx, "mode", 0, "rules")? == "expression";

        if expression_mode {
            let n = param_usize(ctx, "numberOutputs", 0, 4)?.max(1);
            let mut out: Vec<Vec<Item>> = vec![Vec::new(); n];
            for (i, item) in items.iter().enumerate() {
                let idx = param_f64(ctx, "output", i, f64::NAN)?;
                if !idx.is_finite() || idx < 0.0 || idx as usize >= n {
                    return Err(node_err(
                        ctx,
                        format!(
                            "The expression output must be a number between 0 and {} [item {i}]",
                            n - 1
                        ),
                    ));
                }
                out[idx as usize].push(with_pair(item, i));
            }
            return Ok(out);
        }

        let rules = raw_array(ctx, "rules.values");
        let fallback = param(ctx, "options.fallbackOutput", 0)?.unwrap_or(Value::Null);
        let extra = fallback.as_str() == Some("extra");
        let fallback_idx: Option<usize> = match &fallback {
            Value::String(s) if s == "extra" => Some(rules.len()),
            Value::String(s) if s == "none" || s.is_empty() => None,
            Value::String(s) => s.trim().parse().ok(),
            Value::Number(n) => n.as_u64().map(|n| n as usize),
            _ => None,
        };
        let width = rules.len() + usize::from(extra);
        let mut out: Vec<Vec<Item>> = vec![Vec::new(); width.max(1)];

        for (i, item) in items.iter().enumerate() {
            let flags = CondFlags::from_options(ctx, i)?;
            let all = param_bool(ctx, "options.allMatchingOutputs", i, false)?;
            let mut matched = false;
            for (ri, rule) in rules.iter().enumerate() {
                let cond = rule.get("conditions").unwrap_or(&Value::Null);
                if evaluate(ctx, cond, i, flags)? {
                    matched = true;
                    out[ri].push(with_pair(item, i));
                    if !all {
                        break;
                    }
                }
            }
            if !matched
                && let Some(fb) = fallback_idx
            {
                if fb >= out.len() {
                    out.resize_with(fb + 1, Vec::new);
                }
                out[fb].push(with_pair(item, i));
            }
        }
        Ok(out)
    }
}

// ------------------------------------------------------------------------ NoOp

#[derive(Debug)]
pub struct NoOpNode(NodeTypeDescription);

impl Default for NoOpNode {
    fn default() -> Self {
        Self::new()
    }
}

impl NoOpNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "No Operation, do nothing",
            "name": "n8n-nodes-base.noOp",
            "version": [1],
            "description": "No Operation",
            "group": ["organization"],
            "defaults": {"name": "No Operation, do nothing", "color": "#b0b0b0"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": []
        })))
    }
}

#[async_trait]
impl FlowNode for NoOpNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        Ok(vec![port(ctx, 0).to_vec()])
    }
}
