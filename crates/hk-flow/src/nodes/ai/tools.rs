//! Tool sub-nodes (`ai_tool`): Code Tool, Call n8n Workflow Tool, HTTP Request Tool.
//!
//! Tool arguments come from the model. They never become expressions: values that start
//! with `=` are neutralised before they reach a parameter, so a model cannot smuggle
//! `={{ ... }}` into the expression engine.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use super::from_ai::{self, FROM_AI_VAR};
use super::parsers::json_param;
use super::types::AgentTool;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::hooks::{SubWorkflowRequest, SubWorkflowRunner};
use crate::item::Item;
use crate::node::{FlowNode, NodeContext, NodeOutput, Supplied};
use crate::nodes::util::*;

/// Model-facing tool name: `[A-Za-z0-9_-]{1,64}`.
pub(crate) fn tool_name(raw: &str, fallback: &str) -> String {
    let src = if raw.trim().is_empty() { fallback } else { raw };
    let mut n: String = src
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    n.truncate(64);
    if n.is_empty() { "tool".into() } else { n }
}

/// Keep model-provided strings from being read as `=expression`.
fn neutralize(v: &Value) -> Value {
    match v {
        Value::String(s) if s.starts_with('=') => Value::String(format!(" {s}")),
        Value::Array(a) => Value::Array(a.iter().map(neutralize).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, x)| (k.clone(), neutralize(x))).collect())
        }
        other => other.clone(),
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn not_runnable(ctx: &NodeContext) -> FlowError {
    node_err(
        ctx,
        "a tool node feeds an AI node over an `ai_tool` connection and does not run on its own",
    )
}

const SINGLE_INPUT: &str = "input";

fn single_input_schema(what: &str) -> Value {
    json!({"type": "object",
           "properties": {SINGLE_INPUT: {"type": "string", "description": what}},
           "required": [SINGLE_INPUT]})
}

/// The one string a schema-less tool receives, whatever shape the model used.
fn input_text(args: &Value) -> String {
    match args {
        Value::Object(m) => m
            .get(SINGLE_INPUT)
            .map(text_of)
            .unwrap_or_else(|| args.to_string()),
        other => text_of(other),
    }
}

// -------------------------------------------------------------------- code tool

#[derive(Debug)]
struct CodeTool {
    name: String,
    description: String,
    schema: Value,
    /// Model sends named fields (`specifyInputSchema`) instead of one string.
    structured: bool,
    code: String,
    ctx: NodeContext,
}

#[async_trait]
impl AgentTool for CodeTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> Value {
        self.schema.clone()
    }

    async fn call(&self, args: Value) -> Result<String, FlowError> {
        let query = if self.structured {
            args
        } else {
            Value::String(input_text(&args))
        };
        let ctx = self.ctx.with_extra_var("__toolInput", query);
        // brace/string aware template scanner keeps the function body in one expression
        let expr = format!(
            "={{{{ (function() {{ const query = $vars.__toolInput;\n{}\n}})() }}}}",
            self.code
        );
        let out = ctx.eval_param(&Value::String(expr), 0)?;
        Ok(match out {
            Value::Null => String::new(),
            other => text_of(&other),
        })
    }
}

#[derive(Debug)]
pub struct ToolCodeNode(NodeTypeDescription);

impl Default for ToolCodeNode {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolCodeNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Code Tool",
            "name": "@n8n/n8n-nodes-langchain.toolCode",
            "version": [1, 1.1, 1.2, 1.3],
            "defaultVersion": 1.3,
            "description": "Write a tool in JS that the agent can call",
            "group": ["transform"],
            "defaults": {"name": "Code Tool"},
            "inputs": [],
            "outputs": ["ai_tool"],
            "outputNames": ["Tool"],
            "properties": [
                {"displayName": "Name", "name": "name", "type": "string", "default": ""},
                {"displayName": "Description", "name": "description", "type": "string", "default": ""},
                {"displayName": "Language", "name": "language", "type": "options", "default": "javaScript",
                 "options": [{"name": "JavaScript", "value": "javaScript"}, {"name": "Python (Beta)", "value": "python"}]},
                {"displayName": "JavaScript", "name": "jsCode", "type": "string", "default": "",
                 "typeOptions": {"editor": "jsEditor"}, "displayOptions": {"show": {"language": ["javaScript"]}}},
                {"displayName": "Specify Input Schema", "name": "specifyInputSchema", "type": "boolean", "default": false},
                {"displayName": "Schema Type", "name": "schemaType", "type": "options", "default": "fromJson",
                 "options": [{"name": "Generate From JSON Example", "value": "fromJson"}, {"name": "Define Using JSON Schema", "value": "manual"}],
                 "displayOptions": {"show": {"specifyInputSchema": [true]}}},
                {"displayName": "JSON Example", "name": "jsonSchemaExample", "type": "json", "default": "",
                 "displayOptions": {"show": {"schemaType": ["fromJson"], "specifyInputSchema": [true]}}},
                {"displayName": "Input Schema", "name": "inputSchema", "type": "json", "default": "",
                 "displayOptions": {"show": {"schemaType": ["manual"], "specifyInputSchema": [true]}}}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for ToolCodeNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        Err(not_runnable(ctx))
    }

    async fn supply_data(&self, ctx: &NodeContext, i: usize) -> Result<Supplied, FlowError> {
        if param_str(ctx, "language", i, "javaScript")? != "javaScript" {
            return Err(node_err(ctx, "Only JavaScript Code Tools are supported"));
        }
        // raw: the code is not an expression even if it starts with `=`
        let code = raw_param(ctx, "jsCode").map(text_of).unwrap_or_default();
        if code.trim().is_empty() {
            return Err(node_err(ctx, "Code Tool has no code"));
        }
        let structured = param_bool(ctx, "specifyInputSchema", i, false)?;
        let schema = if structured {
            super::parsers::schema_from_params(ctx, i, "inputSchema")?
        } else {
            single_input_schema("Input for the tool")
        };
        let tool: Arc<dyn AgentTool> = Arc::new(CodeTool {
            name: tool_name(&param_str(ctx, "name", i, "")?, &ctx.node.name),
            description: param_str(ctx, "description", i, "")?,
            schema,
            structured,
            code,
            ctx: ctx.clone(),
        });
        Ok(Supplied::new(tool))
    }
}

// -------------------------------------------------------------- workflow tool

#[derive(Debug)]
struct WorkflowTool {
    name: String,
    description: String,
    schema: Value,
    /// Evaluated per call with the model's `$fromAI` values; `None` = pass `{query}`.
    inputs: Option<Value>,
    workflow_id: Option<String>,
    workflow_json: Option<Value>,
    runner: Arc<dyn SubWorkflowRunner>,
    ctx: NodeContext,
}

#[async_trait]
impl AgentTool for WorkflowTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> Value {
        self.schema.clone()
    }

    async fn call(&self, args: Value) -> Result<String, FlowError> {
        let args = neutralize(&args);
        let json = match &self.inputs {
            Some(tpl) => {
                let ctx = self.ctx.with_extra_var(FROM_AI_VAR, args);
                match ctx.eval_param(tpl, 0)? {
                    Value::Object(m) => m,
                    other => Map::from_iter([("value".to_owned(), other)]),
                }
            }
            None => Map::from_iter([("query".to_owned(), Value::String(input_text(&args)))]),
        };
        let items = self
            .runner
            .run(SubWorkflowRequest {
                workflow_id: self.workflow_id.clone(),
                workflow_json: self.workflow_json.clone(),
                items: vec![Item::new(json)],
            })
            .await?;
        let mut jsons = items.into_iter().map(|i| Value::Object(i.json));
        Ok(match (jsons.next(), jsons.next()) {
            (None, _) => String::new(),
            (Some(one), None) => one.to_string(),
            (Some(a), Some(b)) => {
                Value::Array([a, b].into_iter().chain(jsons).collect()).to_string()
            }
        })
    }
}

/// Inputs handed to the sub-workflow, as an expression template: v1.2+ `workflowInputs`
/// mapping, older `fields.values` rows, or nothing.
fn workflow_inputs_template(ctx: &NodeContext) -> Option<Value> {
    if let Some(Value::Object(m)) = raw_param(ctx, "workflowInputs.value")
        && !m.is_empty()
    {
        return Some(Value::Object(m.clone()));
    }
    let rows = raw_array(ctx, "fields.values");
    if rows.is_empty() {
        return None;
    }
    let mut m = Map::new();
    for r in rows {
        let Some(o) = r.as_object() else { continue };
        let Some(name) = o.get("name").and_then(Value::as_str) else {
            continue;
        };
        let val = o
            .iter()
            .find(|(k, _)| k.as_str() != "name" && k.ends_with("Value"))
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Null);
        m.insert(name.to_owned(), val);
    }
    Some(Value::Object(m))
}

#[derive(Debug)]
pub struct ToolWorkflowNode(NodeTypeDescription);

impl Default for ToolWorkflowNode {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolWorkflowNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Call n8n Workflow Tool",
            "name": "@n8n/n8n-nodes-langchain.toolWorkflow",
            "version": [1, 1.1, 1.2, 1.3],
            "defaultVersion": 1.3,
            "description": "Use another workflow as a tool",
            "group": ["transform"],
            "defaults": {"name": "Call n8n Workflow Tool"},
            "inputs": [],
            "outputs": ["ai_tool"],
            "outputNames": ["Tool"],
            "properties": [
                {"displayName": "Name", "name": "name", "type": "string", "default": ""},
                {"displayName": "Description", "name": "description", "type": "string", "default": ""},
                {"displayName": "Source", "name": "source", "type": "options", "default": "database",
                 "options": [{"name": "Database", "value": "database"}, {"name": "Parameter", "value": "parameter"}]},
                {"displayName": "Workflow", "name": "workflowId", "type": "workflowSelector", "default": "",
                 "displayOptions": {"show": {"source": ["database"]}}},
                {"displayName": "Workflow JSON", "name": "workflowJson", "type": "json", "default": "",
                 "displayOptions": {"show": {"source": ["parameter"]}}},
                {"displayName": "Workflow Inputs", "name": "workflowInputs", "type": "resourceMapper", "default": {"mappingMode": "defineBelow", "value": null}}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for ToolWorkflowNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        Err(not_runnable(ctx))
    }

    async fn supply_data(&self, ctx: &NodeContext, i: usize) -> Result<Supplied, FlowError> {
        let runner = ctx
            .hooks
            .sub_workflow
            .clone()
            .ok_or_else(|| node_err(ctx, "Sub-workflows are not available in this runtime"))?;
        let (workflow_id, workflow_json) = match param_str(ctx, "source", i, "database")?.as_str() {
            "parameter" => (None, json_param(ctx, "workflowJson", i)?),
            _ => {
                let id = param(ctx, "workflowId", i)?
                    .as_ref()
                    .and_then(super::model::rl_value)
                    .ok_or_else(|| node_err(ctx, "No workflow selected"))?;
                (Some(id), None)
            }
        };
        let template = workflow_inputs_template(ctx);
        let (schema, inputs) = match &template {
            Some(t) => {
                let calls = from_ai::scan(t);
                (
                    if calls.is_empty() {
                        single_input_schema("Input for the workflow")
                    } else {
                        from_ai::schema_of(&calls)
                    },
                    Some(from_ai::rewrite(t)),
                )
            }
            None => (single_input_schema("Input for the workflow"), None),
        };
        let tool: Arc<dyn AgentTool> = Arc::new(WorkflowTool {
            name: tool_name(&param_str(ctx, "name", i, "")?, &ctx.node.name),
            description: param_str(ctx, "description", i, "")?,
            schema,
            inputs,
            workflow_id,
            workflow_json,
            runner,
            ctx: ctx.clone(),
        });
        Ok(Supplied::new(tool))
    }
}

// ------------------------------------------------------------------- http tool

#[cfg(feature = "http")]
pub use http_tool::ToolHttpRequestNode;

#[cfg(feature = "http")]
mod http_tool {
    use super::*;
    use crate::nodes::HttpRequestNode;
    use crate::workflow::Node;

    #[derive(Debug)]
    struct HttpTool {
        name: String,
        description: String,
        schema: Value,
        /// Placeholders (`{city}`) replaced in the URL and JSON templates.
        placeholders: Vec<String>,
        /// Model-filled fields: `(section, name)`.
        model_fields: Vec<(&'static str, String)>,
        ctx: NodeContext,
    }

    fn fill(tpl: &str, args: &Map<String, Value>, names: &[String], encode: bool) -> String {
        let mut out = tpl.to_owned();
        for n in names {
            let Some(v) = args.get(n) else { continue };
            let raw = text_of(v);
            let rep = if encode {
                raw.bytes()
                    .map(|b| match b {
                        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                            (b as char).to_string()
                        }
                        _ => format!("%{b:02X}"),
                    })
                    .collect()
            } else {
                // JSON string body: escape, drop the surrounding quotes
                let q = Value::String(raw).to_string();
                q[1..q.len() - 1].to_owned()
            };
            out = out.replace(&format!("{{{n}}}"), &rep);
        }
        out
    }

    impl HttpTool {
        /// `{name: value}` object of one section (`Query`/`Headers`/`Body`).
        fn section(
            &self,
            section: &str,
            args: &Map<String, Value>,
        ) -> Result<Map<String, Value>, FlowError> {
            let ctx = &self.ctx;
            let (switch, spec, json_key, params_key) = match section {
                "query" => ("sendQuery", "specifyQuery", "jsonQuery", "parametersQuery"),
                "headers" => (
                    "sendHeaders",
                    "specifyHeaders",
                    "jsonHeaders",
                    "parametersHeaders",
                ),
                _ => ("sendBody", "specifyBody", "jsonBody", "parametersBody"),
            };
            let mut out = Map::new();
            if !param_bool(ctx, switch, 0, false)? {
                return Ok(out);
            }
            match param_str(ctx, spec, 0, "keypair")?.as_str() {
                "json" => {
                    let tpl = param_str(ctx, json_key, 0, "")?;
                    let filled = fill(&tpl, args, &self.placeholders, false);
                    if let Value::Object(m) = serde_json::from_str(&filled).map_err(|e| {
                        node_err(
                            ctx,
                            format!(
                                "`{json_key}` is not valid JSON after filling placeholders: {e}"
                            ),
                        )
                    })? {
                        out = m;
                    }
                }
                "model" => {
                    if let Some(Value::Object(m)) = args.get(section) {
                        out = m.clone();
                    }
                }
                _ => {
                    for row in raw_array(ctx, &format!("{params_key}.values")) {
                        let Some(name) = row.get("name").and_then(Value::as_str) else {
                            continue;
                        };
                        match row
                            .get("valueProvider")
                            .and_then(Value::as_str)
                            .unwrap_or("fieldValue")
                        {
                            "fieldValue" => {
                                let v = row.get("value").cloned().unwrap_or(Value::Null);
                                out.insert(name.to_owned(), ctx.eval_param(&v, 0)?);
                            }
                            _ => {
                                if let Some(v) = args.get(name) {
                                    out.insert(name.to_owned(), v.clone());
                                }
                            }
                        }
                    }
                }
            }
            Ok(out)
        }
    }

    #[async_trait]
    impl AgentTool for HttpTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            &self.description
        }
        fn schema(&self) -> Value {
            self.schema.clone()
        }

        async fn call(&self, args: Value) -> Result<String, FlowError> {
            let args = match neutralize(&args) {
                Value::Object(m) => m,
                // a model that sends a bare string fills the only placeholder, if any
                other => match self
                    .placeholders
                    .first()
                    .or(self.model_fields.first().map(|f| &f.1))
                {
                    Some(k) => Map::from_iter([(k.clone(), other)]),
                    None => Map::new(),
                },
            };
            let ctx = &self.ctx;
            let url_tpl = param_str(ctx, "url", 0, "")?;
            let url = fill(&url_tpl, &args, &self.placeholders, true);
            let mut p = Map::new();
            p.insert("method".into(), param_or(ctx, "method", 0, json!("GET"))?);
            p.insert("url".into(), neutralize(&Value::String(url)));
            for (section, key, spec, json_key) in [
                ("query", "sendQuery", "specifyQuery", "jsonQuery"),
                ("headers", "sendHeaders", "specifyHeaders", "jsonHeaders"),
                ("body", "sendBody", "specifyBody", "jsonBody"),
            ] {
                let m = self.section(section, &args)?;
                if param_bool(ctx, key, 0, false)? {
                    p.insert(key.into(), json!(true));
                    p.insert(spec.into(), json!("json"));
                    p.insert(json_key.into(), neutralize(&Value::Object(m)));
                }
            }
            if param_bool(ctx, "sendBody", 0, false)? {
                p.insert("contentType".into(), json!("json"));
            }
            // credentials are resolved by the HTTP node itself; never copied into params
            for k in ["authentication", "genericAuthType"] {
                if let Some(v) = param(ctx, k, 0)? {
                    p.insert(k.into(), v);
                }
            }
            let mut node = Node::new(
                ctx.node.name.clone(),
                "n8n-nodes-base.httpRequest",
                serde_json::Number::from_f64(4.2).unwrap_or_else(|| 4.into()),
                [0.into(), 0.into()],
            );
            node.parameters = p;
            node.credentials = ctx.node.credentials.clone();
            let c = ctx.with_node(node, vec![vec![Item::default()]]);
            let out = HttpRequestNode::new().execute(&c).await?;
            let items = out.into_iter().next().unwrap_or_default();
            let mut jsons = items.into_iter().map(|i| Value::Object(i.json));
            Ok(match (jsons.next(), jsons.next()) {
                (None, _) => String::new(),
                (Some(one), None) => one.to_string(),
                (Some(a), Some(b)) => {
                    Value::Array([a, b].into_iter().chain(jsons).collect()).to_string()
                }
            })
        }
    }

    /// `{name}` tokens in a template.
    fn placeholders_in(s: &str, out: &mut Vec<String>) {
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'{'
                && let Some(len) = s[i + 1..].find('}')
            {
                let name = &s[i + 1..i + 1 + len];
                if !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !out.iter().any(|o| o == name)
                {
                    out.push(name.to_owned());
                }
            }
            i += 1;
        }
    }

    #[derive(Debug)]
    pub struct ToolHttpRequestNode(NodeTypeDescription);

    impl Default for ToolHttpRequestNode {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ToolHttpRequestNode {
        pub fn new() -> Self {
            Self(describe(json!({
                "displayName": "HTTP Request Tool",
                "name": "@n8n/n8n-nodes-langchain.toolHttpRequest",
                "version": [1, 1.1],
                "defaultVersion": 1.1,
                "description": "Make an HTTP request the agent can call",
                "group": ["transform"],
                "defaults": {"name": "HTTP Request"},
                "inputs": [],
                "outputs": ["ai_tool"],
                "outputNames": ["Tool"],
                "properties": [
                    {"displayName": "Name", "name": "name", "type": "string", "default": ""},
                    {"displayName": "Description", "name": "toolDescription", "type": "string", "default": ""},
                    {"displayName": "Method", "name": "method", "type": "options", "default": "GET",
                     "options": [{"name": "GET", "value": "GET"}, {"name": "POST", "value": "POST"}, {"name": "PUT", "value": "PUT"},
                                 {"name": "PATCH", "value": "PATCH"}, {"name": "DELETE", "value": "DELETE"}]},
                    {"displayName": "URL", "name": "url", "type": "string", "default": ""},
                    {"displayName": "Authentication", "name": "authentication", "type": "options", "default": "none",
                     "options": [{"name": "None", "value": "none"}, {"name": "Generic Credential Type", "value": "genericCredentialType"}]},
                    {"displayName": "Send Query Parameters", "name": "sendQuery", "type": "boolean", "default": false},
                    {"displayName": "Send Headers", "name": "sendHeaders", "type": "boolean", "default": false},
                    {"displayName": "Send Body", "name": "sendBody", "type": "boolean", "default": false},
                    {"displayName": "Placeholder Definitions", "name": "placeholderDefinitions", "type": "fixedCollection", "default": {}}
                ]
            })))
        }
    }

    #[async_trait]
    impl FlowNode for ToolHttpRequestNode {
        fn description(&self) -> &NodeTypeDescription {
            &self.0
        }

        async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
            Err(not_runnable(ctx))
        }

        async fn supply_data(&self, ctx: &NodeContext, i: usize) -> Result<Supplied, FlowError> {
            let mut props = Map::new();
            let mut required: Vec<Value> = Vec::new();
            let mut placeholders = Vec::new();
            // raw templates: placeholders sit in the text the author wrote
            for key in ["url", "jsonQuery", "jsonHeaders", "jsonBody"] {
                if let Some(Value::String(s)) = raw_param(ctx, key) {
                    placeholders_in(s, &mut placeholders);
                }
            }
            let defs = raw_array(ctx, "placeholderDefinitions.values");
            for name in &placeholders {
                let def = defs
                    .iter()
                    .find(|d| d.get("name").and_then(Value::as_str) == Some(name));
                let ty = match def.and_then(|d| d.get("type")).and_then(Value::as_str) {
                    Some(t @ ("number" | "boolean" | "string")) => t,
                    Some("json") => "object",
                    _ => "string",
                };
                let desc = def
                    .and_then(|d| d.get("description"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                props.insert(name.clone(), json!({"type": ty, "description": desc}));
                required.push(json!(name));
            }
            let mut model_fields = Vec::new();
            for (section, switch, spec, params_key) in [
                ("query", "sendQuery", "specifyQuery", "parametersQuery"),
                (
                    "headers",
                    "sendHeaders",
                    "specifyHeaders",
                    "parametersHeaders",
                ),
                ("body", "sendBody", "specifyBody", "parametersBody"),
            ] {
                if !param_bool(ctx, switch, i, false)? {
                    continue;
                }
                match param_str(ctx, spec, i, "keypair")?.as_str() {
                    "model" => {
                        props.insert(section.into(), json!({"type": "object", "description": format!("{section} fields as a JSON object")}));
                    }
                    "json" => {}
                    _ => {
                        for row in raw_array(ctx, &format!("{params_key}.values")) {
                            let (Some(name), provider) = (
                                row.get("name").and_then(Value::as_str),
                                row.get("valueProvider")
                                    .and_then(Value::as_str)
                                    .unwrap_or("fieldValue"),
                            ) else {
                                continue;
                            };
                            if provider == "fieldValue" {
                                continue;
                            }
                            let desc = row.get("value").and_then(Value::as_str).unwrap_or("");
                            props.insert(
                                name.to_owned(),
                                json!({"type": "string", "description": desc}),
                            );
                            if provider == "modelRequired" {
                                required.push(json!(name));
                            }
                            model_fields.push((section, name.to_owned()));
                        }
                    }
                }
            }
            let schema = if props.is_empty() {
                json!({"type": "object", "properties": {}})
            } else {
                json!({"type": "object", "properties": props, "required": required})
            };
            let fallback = ctx.node.name.clone();
            let tool: Arc<dyn AgentTool> = Arc::new(HttpTool {
                name: tool_name(&param_str(ctx, "name", i, "")?, &fallback),
                description: param_str(ctx, "toolDescription", i, "")?,
                schema,
                placeholders,
                model_fields,
                ctx: ctx.clone(),
            });
            Ok(Supplied::new(tool))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_model_safe() {
        assert_eq!(tool_name("", "Order lookup tool"), "Order_lookup_tool");
        assert_eq!(tool_name("calc", "x"), "calc");
        assert_eq!(tool_name("a.b/c", "x"), "a_b_c");
    }

    #[test]
    fn model_equals_prefix_is_neutralised() {
        assert_eq!(
            neutralize(&json!({"a": "={{ 1 }}", "b": ["=x", "y"]})),
            json!({"a": " ={{ 1 }}", "b": [" =x", "y"]})
        );
    }
}
