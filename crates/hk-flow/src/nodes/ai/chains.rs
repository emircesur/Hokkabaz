//! Chain-style root nodes: Basic LLM Chain (`chainLlm`), Text Classifier
//! (`textClassifier`) and Information Extractor (`informationExtractor`). Each makes one
//! model call per input item through the connected `ai_languageModel`.

use std::sync::Arc;

use async_trait::async_trait;
use hk_llm::{Message, Role};
use serde_json::{Map, Value, json};

use super::agent::SubInputs;
use super::parsers::schema_from_params;
use super::schema::{extract_json, json_instructions, validate};
use super::types::ChatModel;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::Item;
use crate::node::{FlowNode, NodeContext, NodeOutput};
use crate::nodes::util::*;

fn msg(role: Role, content: String) -> Message {
    Message { images: Vec::new(),
        role,
        content,
        reasoning: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}

/// One plain completion; chain nodes never offer tools.
async fn ask(
    ctx: &NodeContext,
    model: &Arc<dyn ChatModel>,
    messages: Vec<Message>,
) -> Result<String, FlowError> {
    model
        .complete(messages, Vec::new())
        .await
        .map(|c| c.text)
        .map_err(|e| match e {
            FlowError::Node { message, .. } => node_err(ctx, message),
            other => other,
        })
}

fn chat_input(item: &Item) -> Option<String> {
    ["chatInput", "input"]
        .iter()
        .find_map(|k| item.json.get(*k).map(to_display))
}

// -------------------------------------------------------------- Basic LLM Chain

#[derive(Debug)]
pub struct ChainLlmNode(NodeTypeDescription);

impl Default for ChainLlmNode {
    fn default() -> Self {
        Self::new()
    }
}

impl ChainLlmNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Basic LLM Chain",
            "name": "@n8n/n8n-nodes-langchain.chainLlm",
            "version": [1, 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 1.7, 1.8, 1.9],
            "defaultVersion": 1.9,
            "description": "A simple chain to prompt a large language model",
            "group": ["transform"],
            "defaults": {"name": "Basic LLM Chain", "color": "#909298"},
            "inputs": [
                "main",
                {"type": "ai_languageModel", "displayName": "Model", "required": true, "maxConnections": 1},
                {"type": "ai_outputParser", "displayName": "Output Parser", "maxConnections": 1}
            ],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Source for Prompt (User Message)", "name": "promptType", "type": "options", "default": "auto",
                 "options": [
                    {"name": "Connected Chat Trigger Node", "value": "auto"},
                    {"name": "Define below", "value": "define"}]},
                {"displayName": "Prompt (User Message)", "name": "text", "type": "string", "default": "",
                 "displayOptions": {"show": {"promptType": ["define"]}}},
                {"displayName": "Require Specific Output Format", "name": "hasOutputParser", "type": "boolean", "default": false},
                {"displayName": "Chat Messages (if Using a Chat Model)", "name": "messages", "type": "fixedCollection", "default": {},
                 "typeOptions": {"multipleValues": true}}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for ChainLlmNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        // < 1.5 wrapped the answer as `{response: {text}}`
        let legacy = ctx.node.type_version.as_f64().unwrap_or(1.0) < 1.5;
        let mut out = Vec::new();
        for (i, item) in port(ctx, 0).iter().enumerate() {
            let inputs = SubInputs::resolve(ctx, i).await?;
            let model = inputs.require_model(ctx)?;
            let prompt = match param_str(ctx, "promptType", i, "")?.as_str() {
                "define" => param_str(ctx, "text", i, "")?,
                "auto" => chat_input(item).ok_or_else(|| {
                    node_err(
                        ctx,
                        "No prompt specified: expected a field `chatInput` in the input item",
                    )
                })?,
                // < 1.4: `prompt`; 1.4 without promptType keeps `text`
                _ => match param(ctx, "prompt", i)? {
                    Some(p) => to_display(&p),
                    None => param_str(ctx, "text", i, "")?,
                },
            };
            let mut msgs = Vec::new();
            let mut system = Vec::new();
            for row in raw_array(ctx, "messages.messageValues") {
                let text = match row.get("message") {
                    Some(m) => to_display(&ctx.eval_param(m, i)?),
                    None => continue,
                };
                match row
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("HumanMessagePromptTemplate")
                {
                    "SystemMessagePromptTemplate" => system.push(text),
                    "AIMessagePromptTemplate" => msgs.push(msg(Role::Assistant, text)),
                    _ => msgs.push(msg(Role::User, text)),
                }
            }
            if let Some(p) = &inputs.parser {
                system.push(p.format_instructions());
            }
            let mut all = Vec::new();
            if !system.is_empty() {
                all.push(msg(Role::System, system.join("\n\n")));
            }
            all.extend(msgs);
            all.push(msg(Role::User, prompt));
            let text = ask(ctx, &model, all).await?;
            let value = match &inputs.parser {
                Some(p) => p.parse(&text).map_err(|e| match e {
                    FlowError::Node { message, .. } => node_err(ctx, message),
                    other => other,
                })?,
                None => Value::String(text),
            };
            let mut m = Map::new();
            if legacy {
                m.insert("response".into(), json!({"text": value}));
            } else {
                m.insert("text".into(), value);
            }
            let mut o = Item::new(m);
            o.paired_item = paired(i);
            out.push(o);
        }
        Ok(vec![out])
    }
}

// ------------------------------------------------------------- Text Classifier

#[derive(Debug)]
pub struct TextClassifierNode(NodeTypeDescription);

impl Default for TextClassifierNode {
    fn default() -> Self {
        Self::new()
    }
}

impl TextClassifierNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Text Classifier",
            "name": "@n8n/n8n-nodes-langchain.textClassifier",
            "version": [1, 1.1],
            "defaultVersion": 1.1,
            "description": "Classify your text into distinct categories",
            "group": ["transform"],
            "defaults": {"name": "Text Classifier"},
            "inputs": [
                "main",
                {"type": "ai_languageModel", "displayName": "Model", "required": true, "maxConnections": 1}
            ],
            // one output per category (+ "Other" with fallback=other)
            "outputs": "={{ (() => { const c = ($parameter.categories?.categories ?? []).map(x => ({ type: 'main', displayName: x.category })); return $parameter.options?.fallback === 'other' ? [...c, { type: 'main', displayName: 'Other' }] : c; })() }}",
            "properties": [
                {"displayName": "Text to Classify", "name": "inputText", "type": "string", "default": ""},
                {"displayName": "Categories", "name": "categories", "type": "fixedCollection", "default": {},
                 "typeOptions": {"multipleValues": true}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add Option",
                 "options": [
                    {"displayName": "Allow Multiple Classes To Be True", "name": "multiClass", "type": "boolean", "default": false},
                    {"displayName": "When No Clear Match", "name": "fallback", "type": "options", "default": "discard",
                     "options": [{"name": "Discard Item", "value": "discard"}, {"name": "Output on Extra, 'Other' Branch", "value": "other"}]},
                    {"displayName": "System Prompt Template", "name": "systemPromptTemplate", "type": "string", "default": ""}]}
            ]
        })))
    }
}

struct Category {
    name: String,
    description: String,
}

fn categories(ctx: &NodeContext, i: usize) -> Result<Vec<Category>, FlowError> {
    let mut out = Vec::new();
    for row in raw_array(ctx, "categories.categories") {
        let get = |k: &str| match row.get(k) {
            Some(v) => ctx.eval_param(v, i).map(|v| to_display(&v)),
            None => Ok(String::new()),
        };
        let name = get("category")?;
        if !name.is_empty() {
            out.push(Category {
                name,
                description: get("description")?,
            });
        }
    }
    if out.is_empty() {
        return Err(node_err(ctx, "At least one category is required"));
    }
    Ok(out)
}

/// Category names the model picked. Accepts `{"categories": [..]}`, `{"category": ".."}`,
/// a `{name: bool}` map, or bare text that is exactly (or contains) one name.
fn picked(text: &str, cats: &[Category], multi: bool) -> Vec<usize> {
    let find = |s: &str| {
        cats.iter()
            .position(|c| c.name.eq_ignore_ascii_case(s.trim()))
    };
    let mut hits: Vec<usize> = Vec::new();
    match extract_json(text) {
        Some(Value::Object(m)) => {
            let listed = m.get("categories").or_else(|| m.get("category"));
            match listed {
                Some(Value::Array(a)) => {
                    hits.extend(a.iter().filter_map(Value::as_str).filter_map(find))
                }
                Some(Value::String(s)) => hits.extend(find(s)),
                _ => hits.extend(
                    m.iter()
                        .filter(|(_, v)| **v == Value::Bool(true))
                        .filter_map(|(k, _)| find(k)),
                ),
            }
        }
        Some(Value::Array(a)) => hits.extend(a.iter().filter_map(Value::as_str).filter_map(find)),
        Some(Value::String(s)) => hits.extend(find(&s)),
        _ => {
            hits.extend(find(text));
            if hits.is_empty() {
                let lower = text.to_lowercase();
                let found: Vec<usize> = cats
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| lower.contains(&c.name.to_lowercase()))
                    .map(|(i, _)| i)
                    .collect();
                if found.len() == 1 {
                    hits = found;
                }
            }
        }
    }
    hits.dedup();
    if !multi {
        hits.truncate(1);
    }
    hits
}

#[async_trait]
impl FlowNode for TextClassifierNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let mut out: Vec<Vec<Item>> = Vec::new();
        for (i, item) in port(ctx, 0).iter().enumerate() {
            let cats = categories(ctx, i)?;
            if out.is_empty() {
                let other = param_str(ctx, "options.fallback", i, "discard")? == "other";
                out = vec![Vec::new(); cats.len() + usize::from(other)];
            }
            let inputs = SubInputs::resolve(ctx, i).await?;
            let model = inputs.require_model(ctx)?;
            let multi = param_bool(ctx, "options.multiClass", i, false)?;
            let text = param_str(ctx, "inputText", i, "")?;
            if text.trim().is_empty() {
                return Err(node_err(ctx, "No text to classify (`inputText` is empty)"));
            }
            let list: String = cats
                .iter()
                .map(|c| {
                    if c.description.is_empty() {
                        format!("- {}", c.name)
                    } else {
                        format!("- {}: {}", c.name, c.description)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let custom = param_str(ctx, "options.systemPromptTemplate", i, "")?;
            let system = format!(
                "{}\n\nCategories:\n{list}\n\n{}\nRespond only with JSON of the form {{\"categories\": [<category names>]}}. If nothing fits, use an empty list.",
                if custom.is_empty() {
                    "Classify the text provided by the user into the categories below."
                } else {
                    custom.as_str()
                },
                if multi {
                    "Several categories may apply."
                } else {
                    "Choose exactly one category."
                }
            );
            let reply = ask(
                ctx,
                &model,
                vec![msg(Role::System, system), msg(Role::User, text)],
            )
            .await?;
            let hits = picked(&reply, &cats, multi);
            let mut it = item.clone();
            it.paired_item = paired(i);
            if hits.is_empty() {
                // "Other" branch is the extra last output
                if out.len() > cats.len() {
                    out[cats.len()].push(it);
                }
            } else {
                for h in hits {
                    out[h].push(it.clone());
                }
            }
        }
        Ok(out)
    }
}

// --------------------------------------------------------- Information Extractor

#[derive(Debug)]
pub struct InformationExtractorNode(NodeTypeDescription);

impl Default for InformationExtractorNode {
    fn default() -> Self {
        Self::new()
    }
}

impl InformationExtractorNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Information Extractor",
            "name": "@n8n/n8n-nodes-langchain.informationExtractor",
            "version": [1, 1.1, 1.2],
            "defaultVersion": 1.2,
            "description": "Extract information from text in a structured format",
            "group": ["transform"],
            "defaults": {"name": "Information Extractor"},
            "inputs": [
                "main",
                {"type": "ai_languageModel", "displayName": "Model", "required": true, "maxConnections": 1}
            ],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Text", "name": "text", "type": "string", "default": ""},
                {"displayName": "Schema Type", "name": "schemaType", "type": "options", "default": "fromAttributes",
                 "options": [
                    {"name": "From Attribute Descriptions", "value": "fromAttributes"},
                    {"name": "Generate From JSON Example", "value": "fromJson"},
                    {"name": "Define Below", "value": "manual"}]},
                {"displayName": "Attributes", "name": "attributes", "type": "fixedCollection", "default": {},
                 "typeOptions": {"multipleValues": true}, "displayOptions": {"show": {"schemaType": ["fromAttributes"]}}},
                {"displayName": "JSON Example", "name": "jsonSchemaExample", "type": "json", "default": "",
                 "displayOptions": {"show": {"schemaType": ["fromJson"]}}},
                {"displayName": "Input Schema", "name": "inputSchema", "type": "json", "default": "",
                 "displayOptions": {"show": {"schemaType": ["manual"]}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add Option",
                 "options": [{"displayName": "System Prompt Template", "name": "systemPromptTemplate", "type": "string", "default": ""}]}
            ]
        })))
    }
}

fn attributes_schema(ctx: &NodeContext, i: usize) -> Result<Value, FlowError> {
    let mut props = Map::new();
    let mut required = Vec::new();
    for row in raw_array(ctx, "attributes.attributes") {
        let get = |k: &str| match row.get(k) {
            Some(v) => ctx.eval_param(v, i),
            None => Ok(Value::Null),
        };
        let name = to_display(&get("name")?);
        if name.is_empty() {
            continue;
        }
        let desc = to_display(&get("description")?);
        let mut p = match get("type")?.as_str().unwrap_or("string") {
            "number" => json!({"type": "number"}),
            "boolean" => json!({"type": "boolean"}),
            "date" => json!({"type": "string", "format": "date"}),
            "array" => json!({"type": "array", "items": {"type": "string"}}),
            _ => json!({"type": "string"}),
        };
        if !desc.is_empty() {
            p["description"] = Value::String(desc);
        }
        props.insert(name.clone(), p);
        if get("required")? == Value::Bool(true) {
            required.push(Value::String(name));
        }
    }
    if props.is_empty() {
        return Err(node_err(ctx, "At least one attribute is required"));
    }
    Ok(json!({"type": "object", "properties": props, "required": required}))
}

#[async_trait]
impl FlowNode for InformationExtractorNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let mut out = Vec::new();
        for (i, _) in port(ctx, 0).iter().enumerate() {
            let inputs = SubInputs::resolve(ctx, i).await?;
            let model = inputs.require_model(ctx)?;
            let schema = match param_str(ctx, "schemaType", i, "fromAttributes")?.as_str() {
                "fromAttributes" => attributes_schema(ctx, i)?,
                _ => schema_from_params(ctx, i, "inputSchema")?,
            };
            let text = param_str(ctx, "text", i, "")?;
            let custom = param_str(ctx, "options.systemPromptTemplate", i, "")?;
            let system = format!(
                "{}\n\n{}",
                if custom.is_empty() {
                    "You are an expert extraction algorithm. Only extract relevant information from the text. \
                     If you do not know the value of an attribute asked to extract, omit it or use null."
                } else {
                    custom.as_str()
                },
                json_instructions(&schema)
            );
            let reply = ask(
                ctx,
                &model,
                vec![msg(Role::System, system), msg(Role::User, text)],
            )
            .await?;
            let v = extract_json(&reply)
                .ok_or_else(|| node_err(ctx, "Model output is not valid JSON"))?;
            // `null` for an omitted optional attribute must not fail type checks
            let v = match v {
                Value::Object(m) => {
                    Value::Object(m.into_iter().filter(|(_, x)| !x.is_null()).collect())
                }
                other => other,
            };
            validate(&schema, &v).map_err(|e| {
                node_err(ctx, format!("Model output does not match the schema: {e}"))
            })?;
            let mut m = Map::new();
            m.insert("output".into(), v);
            let mut o = Item::new(m);
            o.paired_item = paired(i);
            out.push(o);
        }
        Ok(vec![out])
    }
}
