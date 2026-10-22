//! Code node (`n8n-nodes-base.code` v1, v2): JavaScript in the QuickJS sandbox that also
//! runs expressions (no fs/net/process, memory/stack/time limits). Python is not
//! supported and fails with a clear node error.
//!
//! Modes: `runOnceForAllItems` (script returns the output items) and `runOnceForEachItem`
//! (script runs per input item, returns one object, or `null` to drop it).

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use super::util::*;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::expr::{ExprError, ScriptMode};
use crate::item::{Item, PairedItem};
use crate::node::{FlowNode, NodeContext, NodeOutput};

const NOT_ITEMS: &str = "Code doesn't return items properly. \
     Please return an array of objects, one for each item you would like to output.";
const NOT_OBJECT: &str = "Code doesn't return an object. \
     Please return an object (or null to drop the item) for each input item.";

const DEFAULT_JS: &str = "// Loop over input items and add a new field called 'myNewField' to the JSON of each one\n\
for (const item of $input.all()) {\n  item.json.myNewField = 1;\n}\n\nreturn $input.all();";

#[derive(Debug)]
pub struct CodeNode(NodeTypeDescription);

impl Default for CodeNode {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Code",
            "name": "n8n-nodes-base.code",
            "version": [1, 2],
            "defaultVersion": 2,
            "description": "Run custom JavaScript code",
            "subtitle": "={{ $parameter[\"language\"] === \"python\" ? \"Python\" : \"JavaScript\" }}",
            "group": ["transform"],
            "defaults": {"name": "Code", "color": "#FF9922"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Mode", "name": "mode", "type": "options", "default": "runOnceForAllItems",
                 "options": [
                    {"name": "Run Once for All Items", "value": "runOnceForAllItems",
                     "description": "Run this code only once, no matter how many input items there are"},
                    {"name": "Run Once for Each Item", "value": "runOnceForEachItem",
                     "description": "Run this code as many times as there are input items"}]},
                {"displayName": "Language", "name": "language", "type": "options", "default": "javaScript",
                 "options": [
                    {"name": "JavaScript", "value": "javaScript"},
                    {"name": "Python (Beta)", "value": "python"}]},
                {"displayName": "JavaScript", "name": "jsCode", "type": "string", "default": DEFAULT_JS,
                 "typeOptions": {"editor": "codeNodeEditor", "editorLanguage": "javaScript"},
                 "displayOptions": {"show": {"language": ["javaScript"]}}},
                {"displayName": "Python", "name": "pythonCode", "type": "string", "default": "",
                 "typeOptions": {"editor": "codeNodeEditor", "editorLanguage": "python"},
                 "displayOptions": {"show": {"language": ["python", "pythonNative"]}}}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for CodeNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        // Mode and language are plain options; the code is never expression-resolved (a
        // script starting with `=` is code, not an expression).
        let text = |path: &str, default: &str| match raw_param(ctx, path) {
            Some(Value::String(s)) => s.clone(),
            _ => default.to_owned(),
        };
        let language = text("language", "javaScript");
        if language == "python" || language == "pythonNative" {
            return self.execute_python(ctx).await;
        }
        if language != "javaScript" {
            return Err(node_err(ctx, format!("Unknown language '{language}'")));
        }
        let mode = match text("mode", "runOnceForAllItems").as_str() {
            "runOnceForAllItems" => ScriptMode::AllItems,
            "runOnceForEachItem" => ScriptMode::EachItem,
            other => return Err(node_err(ctx, format!("Unknown Code node mode '{other}'"))),
        };
        let source = text("jsCode", "");
        let input = port(ctx, 0);
        if mode == ScriptMode::EachItem && input.is_empty() {
            return Ok(vec![Vec::new()]);
        }

        let evaluator = ctx.evaluator().map_err(|e| node_err(ctx, e.to_string()))?;
        let out = evaluator
            .run_script(&source, mode, &ctx.expr_scope(0))
            .map_err(|e| node_err(ctx, script_failure(e)))?;
        for line in &out.logs {
            tracing::debug!(node = %ctx.node.name, "console: {line}");
        }

        let items = match mode {
            ScriptMode::AllItems => {
                if !out.undefined.is_empty() {
                    return Err(node_err(ctx, NOT_ITEMS));
                }
                all_items(out.value).map_err(|m| node_err(ctx, m))?
            }
            ScriptMode::EachItem => {
                each_items(out.value, &out.undefined).map_err(|m| node_err(ctx, m))?
            }
        };
        Ok(vec![items])
    }
}

impl CodeNode {
    async fn execute_python(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        if !super::python::allowed(ctx.hooks.allow_python) {
            return Err(node_err(
                ctx,
                "Python runs as an unsandboxed process on this device, so it is off by default. \
                 Set HOKKABAZ_ALLOW_PYTHON=1 to enable it, or switch the node to JavaScript.",
            ));
        }
        let each = match raw_param(ctx, "mode") {
            Some(Value::String(s)) => s == "runOnceForEachItem",
            _ => false,
        };
        let code = match raw_param(ctx, "pythonCode") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let input = port(ctx, 0);
        if each && input.is_empty() {
            return Ok(vec![Vec::new()]);
        }
        let items: Vec<Value> = input.iter().map(|i| json!({"json": i.json})).collect();
        let out = super::python::run(&code, each, items).await.map_err(|m| node_err(ctx, m))?;
        let items = if each {
            let Value::Array(results) = out else { return Err(node_err(ctx, NOT_OBJECT)) };
            let mut v = Vec::new();
            for (i, r) in results.into_iter().enumerate() {
                match r {
                    Value::Null => {}
                    Value::Object(_) => {
                        let mut item = to_item(r).map_err(|m| node_err(ctx, at_item(&m, i)))?;
                        item.paired_item = Some(PairedItem::Index(i));
                        v.push(item);
                    }
                    _ => return Err(node_err(ctx, at_item(NOT_OBJECT, i))),
                }
            }
            v
        } else {
            all_items(out).map_err(|m| node_err(ctx, m))?
        };
        Ok(vec![items])
    }
}

fn script_failure(e: ExprError) -> String {
    match e {
        ExprError::Timeout => {
            "Code execution timed out: the script ran longer than the limit".into()
        }
        ExprError::Memory => "Code execution exceeded the memory limit".into(),
        other => other.to_string(),
    }
}

/// Error text for a problem with output item `i` (`[for item i]` suffix like script errors).
fn at_item(message: &str, i: usize) -> String {
    ExprError::Script {
        message: message.to_owned(),
        line: None,
        item_index: Some(i),
    }
    .to_string()
}

/// `{json: {...}, binary?, pairedItem?}` form (as opposed to a plain data object).
fn is_item_form(v: &Value) -> bool {
    v.as_object().is_some_and(|m| m.contains_key("json"))
}

fn all_items(v: Value) -> Result<Vec<Item>, String> {
    let list = match v {
        Value::Null => return Ok(Vec::new()),
        Value::Array(a) => a,
        obj @ Value::Object(_) => vec![obj],
        _ => return Err(NOT_ITEMS.into()),
    };
    let with_json = list.iter().filter(|x| is_item_form(x)).count();
    if with_json != 0 && with_json != list.len() {
        return Err(
            "Code doesn't return items properly. If you return an item with the key \
                    `json`, all returned items need it."
                .into(),
        );
    }
    list.into_iter()
        .enumerate()
        .map(|(i, v)| to_item(v).map_err(|m| at_item(&m, i)))
        .collect()
}

fn each_items(v: Value, undefined: &[usize]) -> Result<Vec<Item>, String> {
    let Value::Array(results) = v else {
        return Err(NOT_OBJECT.into());
    };
    let mut out = Vec::with_capacity(results.len());
    for (i, v) in results.into_iter().enumerate() {
        if undefined.contains(&i) {
            return Err(at_item(NOT_OBJECT, i));
        }
        match v {
            Value::Null => {}
            Value::Object(_) => {
                let mut item = to_item(v).map_err(|m| at_item(&m, i))?;
                if item.paired_item.is_none() {
                    item.paired_item = Some(PairedItem::Index(i));
                }
                out.push(item);
            }
            _ => return Err(at_item(NOT_OBJECT, i)),
        }
    }
    Ok(out)
}

fn to_item(v: Value) -> Result<Item, String> {
    match v {
        Value::Object(m) if m.contains_key("json") => {
            if !matches!(m.get("json"), Some(Value::Object(_))) {
                return Err("A 'json' property isn't an object".into());
            }
            // Only the three n8n item keys survive; anything else is script scratch.
            let mut keep = Map::new();
            for k in ["json", "binary", "pairedItem"] {
                if let Some(x) = m.get(k) {
                    keep.insert(k.to_owned(), x.clone());
                }
            }
            serde_json::from_value::<Item>(Value::Object(keep))
                .map_err(|e| format!("Returned item is malformed: {e}"))
        }
        Value::Object(m) => Ok(Item::new(m)),
        _ => {
            Err("Code doesn't return items properly. Every returned item must be an object".into())
        }
    }
}
