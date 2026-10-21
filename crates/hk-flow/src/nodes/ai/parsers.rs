//! Output parsers (`ai_outputParser`): Structured Output Parser and Item List Output Parser.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::schema::{extract_json, json_instructions, schema_from_example, validate};
use super::types::{OutputParser, ai_err};
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::node::{FlowNode, NodeContext, NodeOutput, Supplied};
use crate::nodes::util::*;

// ------------------------------------------------------------------ structured

/// JSON-Schema-checked JSON.
#[derive(Debug)]
pub struct StructuredParser {
    pub schema: Value,
}

impl OutputParser for StructuredParser {
    fn format_instructions(&self) -> String {
        json_instructions(&self.schema)
    }

    fn parse(&self, text: &str) -> Result<Value, FlowError> {
        let mut v = extract_json(text).ok_or_else(|| {
            ai_err("Model output is not valid JSON; cannot apply the output parser")
        })?;
        // models sometimes wrap the payload in `{"output": ...}` (n8n's own wrapper)
        let declares_output = self
            .schema
            .get("properties")
            .and_then(|p| p.get("output"))
            .is_some();
        if !declares_output
            && let Value::Object(m) = &v
            && m.len() == 1
            && let Some(inner) = m.get("output")
            && validate(&self.schema, inner).is_ok()
        {
            v = inner.clone();
        }
        validate(&self.schema, &v)
            .map_err(|e| ai_err(format!("Model output does not match the schema: {e}")))?;
        Ok(v)
    }
}

#[derive(Debug)]
pub struct StructuredOutputParserNode(NodeTypeDescription);

impl Default for StructuredOutputParserNode {
    fn default() -> Self {
        Self::new()
    }
}

impl StructuredOutputParserNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Structured Output Parser",
            "name": "@n8n/n8n-nodes-langchain.outputParserStructured",
            "version": [1, 1.1, 1.2, 1.3],
            "defaultVersion": 1.3,
            "description": "Return data in a defined JSON format",
            "group": ["transform"],
            "defaults": {"name": "Structured Output Parser"},
            "inputs": [],
            "outputs": ["ai_outputParser"],
            "outputNames": ["Output Parser"],
            "properties": [
                {"displayName": "Schema Type", "name": "schemaType", "type": "options", "default": "fromJson",
                 "options": [
                    {"name": "Generate From JSON Example", "value": "fromJson"},
                    {"name": "Define Below", "value": "manual"}]},
                {"displayName": "JSON Example", "name": "jsonSchemaExample", "type": "json", "default": "{\n\t\"state\": \"California\"\n}",
                 "displayOptions": {"show": {"schemaType": ["fromJson"]}}},
                {"displayName": "Input Schema", "name": "inputSchema", "type": "json", "default": "{\n\"type\": \"object\",\n\"properties\": {}\n}",
                 "displayOptions": {"show": {"schemaType": ["manual"]}}},
                {"displayName": "Auto-Fix Format", "name": "autoFix", "type": "boolean", "default": false}
            ]
        })))
    }
}

/// JSON parameter given as an object or as a JSON string.
pub(crate) fn json_param(
    ctx: &NodeContext,
    path: &str,
    i: usize,
) -> Result<Option<Value>, FlowError> {
    match param(ctx, path, i)? {
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => serde_json::from_str(&s)
            .map(Some)
            .map_err(|e| node_err(ctx, format!("`{path}` is not valid JSON: {e}"))),
        Some(Value::Null) | None => Ok(None),
        Some(v) => Ok(Some(v)),
    }
}

/// Schema from `schemaType` + `jsonSchemaExample` / `inputSchema` (shared with the
/// Information Extractor).
pub(crate) fn schema_from_params(
    ctx: &NodeContext,
    i: usize,
    manual_key: &str,
) -> Result<Value, FlowError> {
    // older versions have no schemaType: manual schema when given, else the example
    let kind = param_str(ctx, "schemaType", i, "")?;
    if kind == "manual" || (kind.is_empty() && raw_param(ctx, manual_key).is_some()) {
        json_param(ctx, manual_key, i)?.ok_or_else(|| node_err(ctx, "Input schema is empty"))
    } else {
        let ex = json_param(ctx, "jsonSchemaExample", i)?
            .ok_or_else(|| node_err(ctx, "JSON example is empty"))?;
        Ok(schema_from_example(&ex))
    }
}

#[async_trait]
impl FlowNode for StructuredOutputParserNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        Err(node_err(
            ctx,
            "an output parser feeds an AI node over an `ai_outputParser` connection and does not run on its own",
        ))
    }

    async fn supply_data(&self, ctx: &NodeContext, i: usize) -> Result<Supplied, FlowError> {
        let schema = schema_from_params(ctx, i, "inputSchema")?;
        let p: Arc<dyn OutputParser> = Arc::new(StructuredParser { schema });
        Ok(Supplied::new(p))
    }
}

// ------------------------------------------------------------------- item list

/// Splits the answer into a list of strings.
#[derive(Debug)]
pub struct ItemListParser {
    pub separator: String,
    /// `None` = any count.
    pub count: Option<usize>,
}

impl OutputParser for ItemListParser {
    fn format_instructions(&self) -> String {
        let sep = if self.separator == "\n" {
            "a new line".to_owned()
        } else {
            format!("`{}`", self.separator)
        };
        match self.count {
            Some(n) => format!(
                "Your response should be a list of {n} items separated by {sep} (eg: `foo{}bar{}baz`)",
                self.separator, self.separator
            ),
            None => format!(
                "Your response should be a list of items separated by {sep} (eg: `foo{}bar{}baz`)",
                self.separator, self.separator
            ),
        }
    }

    fn parse(&self, text: &str) -> Result<Value, FlowError> {
        let items: Vec<Value> = text
            .trim()
            .split(self.separator.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| Value::String(s.to_owned()))
            .collect();
        if let Some(n) = self.count {
            if items.len() < n {
                return Err(ai_err(format!(
                    "Wrong number of items returned: expected {n}, got {}",
                    items.len()
                )));
            }
            return Ok(Value::Array(items.into_iter().take(n).collect()));
        }
        Ok(Value::Array(items))
    }
}

#[derive(Debug)]
pub struct ItemListOutputParserNode(NodeTypeDescription);

impl Default for ItemListOutputParserNode {
    fn default() -> Self {
        Self::new()
    }
}

impl ItemListOutputParserNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Item List Output Parser",
            "name": "@n8n/n8n-nodes-langchain.outputParserItemList",
            "version": 1,
            "description": "Return the results as separate items",
            "group": ["transform"],
            "defaults": {"name": "Item List Output Parser"},
            "inputs": [],
            "outputs": ["ai_outputParser"],
            "outputNames": ["Output Parser"],
            "properties": [
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add Option",
                 "options": [
                    {"displayName": "Number of Items", "name": "numberOfItems", "type": "number", "default": -1},
                    {"displayName": "Separator", "name": "separator", "type": "string", "default": "\n"}]}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for ItemListOutputParserNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        Err(node_err(
            ctx,
            "an output parser feeds an AI node over an `ai_outputParser` connection and does not run on its own",
        ))
    }

    async fn supply_data(&self, ctx: &NodeContext, i: usize) -> Result<Supplied, FlowError> {
        let n = param_f64(ctx, "options.numberOfItems", i, -1.0)?;
        let p: Arc<dyn OutputParser> = Arc::new(ItemListParser {
            separator: param_str(ctx, "options.separator", i, "\n")?,
            count: (n >= 1.0).then_some(n as usize),
        });
        Ok(Supplied::new(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_unwraps_and_validates() {
        let p = StructuredParser {
            schema: json!({"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}),
        };
        assert_eq!(
            p.parse("```json\n{\"city\":\"Rome\"}\n```").unwrap(),
            json!({"city": "Rome"})
        );
        assert_eq!(
            p.parse("{\"output\":{\"city\":\"Rome\"}}").unwrap(),
            json!({"city": "Rome"})
        );
        assert!(p.parse("{\"city\": 4}").is_err());
        assert!(p.parse("no json").is_err());
    }

    #[test]
    fn item_list() {
        let p = ItemListParser {
            separator: "\n".into(),
            count: Some(2),
        };
        assert_eq!(p.parse("a\n b \n\nc").unwrap(), json!(["a", "b"]));
        assert!(p.parse("only").is_err());
    }
}
