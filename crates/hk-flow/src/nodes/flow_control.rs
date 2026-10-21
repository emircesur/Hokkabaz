//! Wait (short in-process), Execute Workflow (hook), Respond to Webhook (hook).

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Value, json};

use super::date::{now_millis, parse_date, parse_zone};
use super::util::*;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::hooks::{ResponseBody, SubWorkflowRequest, WebhookResponse};
use crate::item::Item;
use crate::node::{FlowNode, NodeContext, NodeOutput};
use crate::triggers::wait::{WaitRequest, WaitTrigger};

// ------------------------------------------------------------------------ Wait

#[derive(Debug)]
pub struct WaitNode(NodeTypeDescription);

impl Default for WaitNode {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Wait",
            "name": "n8n-nodes-base.wait",
            "version": [1, 1.1],
            "defaultVersion": 1.1,
            "description": "Wait before continuing with execution",
            "group": ["organization"],
            "defaults": {"name": "Wait", "color": "#804050"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Resume", "name": "resume", "type": "options", "default": "timeInterval",
                 "options": [
                    {"name": "After Time Interval", "value": "timeInterval"},
                    {"name": "At Specified Time", "value": "specificTime"},
                    {"name": "On Webhook Call", "value": "webhook"},
                    {"name": "On Form Submitted", "value": "form"}]},
                {"displayName": "Date and Time", "name": "dateTime", "type": "dateTime", "default": "",
                 "displayOptions": {"show": {"resume": ["specificTime"]}}},
                {"displayName": "Wait Amount", "name": "amount", "type": "number", "default": 1,
                 "displayOptions": {"show": {"resume": ["timeInterval"]}}},
                {"displayName": "Wait Unit", "name": "unit", "type": "options", "default": "hours",
                 "displayOptions": {"show": {"resume": ["timeInterval"]}},
                 "options": [
                    {"name": "Seconds", "value": "seconds"},
                    {"name": "Minutes", "value": "minutes"},
                    {"name": "Hours", "value": "hours"},
                    {"name": "Days", "value": "days"}]},
                {"displayName": "HTTP Method", "name": "httpMethod", "type": "options", "default": "GET",
                 "displayOptions": {"show": {"resume": ["webhook"]}},
                 "options": [
                    {"name": "DELETE", "value": "DELETE"}, {"name": "GET", "value": "GET"},
                    {"name": "HEAD", "value": "HEAD"}, {"name": "PATCH", "value": "PATCH"},
                    {"name": "POST", "value": "POST"}, {"name": "PUT", "value": "PUT"}]},
                {"displayName": "Limit Wait Time", "name": "limitWaitTime", "type": "boolean", "default": false,
                 "displayOptions": {"show": {"resume": ["webhook", "form"]}}},
                {"displayName": "Limit Type", "name": "limitType", "type": "options", "default": "afterTimeInterval",
                 "displayOptions": {"show": {"limitWaitTime": [true], "resume": ["webhook", "form"]}},
                 "options": [
                    {"name": "After Time Interval", "value": "afterTimeInterval"},
                    {"name": "At Specified Time", "value": "atSpecifiedTime"}]},
                {"displayName": "Amount", "name": "resumeAmount", "type": "number", "default": 1,
                 "displayOptions": {"show": {"limitType": ["afterTimeInterval"], "limitWaitTime": [true], "resume": ["webhook", "form"]}}},
                {"displayName": "Unit", "name": "resumeUnit", "type": "options", "default": "hours",
                 "displayOptions": {"show": {"limitType": ["afterTimeInterval"], "limitWaitTime": [true], "resume": ["webhook", "form"]}},
                 "options": [
                    {"name": "Seconds", "value": "seconds"}, {"name": "Minutes", "value": "minutes"},
                    {"name": "Hours", "value": "hours"}, {"name": "Days", "value": "days"}]},
                {"displayName": "Max Date and Time", "name": "maxDateAndTime", "type": "dateTime", "default": "",
                 "displayOptions": {"show": {"limitType": ["atSpecifiedTime"], "limitWaitTime": [true], "resume": ["webhook", "form"]}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "displayOptions": {"show": {"resume": ["webhook"]}},
                 "options": [
                    {"displayName": "Webhook Suffix", "name": "webhookSuffix", "type": "string", "default": ""}]}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for WaitNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    /// Short waits sleep in-process. Longer ones, and webhook/form waits, suspend the
    /// execution when the executor can park it (`hooks.durable_wait`).
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let now = ctx.expr.now_ms.unwrap_or_else(now_millis);
        let mode = param_str(ctx, "resume", 0, "timeInterval")?;
        let request = match mode.as_str() {
            "timeInterval" => {
                let amount = param_f64(ctx, "amount", 0, 1.0)?;
                let ms = amount * unit_ms(&param_str(ctx, "unit", 0, "hours")?);
                WaitRequest {
                    resume_at: Some(now.saturating_add(ms.max(0.0) as i64)),
                    on: WaitTrigger::Time,
                }
            }
            "specificTime" => {
                let v = param_or(ctx, "dateTime", 0, Value::Null)?;
                let tz = parse_zone(ctx.expr.timezone.as_deref().unwrap_or("UTC"));
                let target = parse_date(&v, tz)
                    .ok_or_else(|| node_err(ctx, "Invalid date for 'Date and Time'"))?;
                WaitRequest {
                    resume_at: Some(target.timestamp_millis()),
                    on: WaitTrigger::Time,
                }
            }
            "webhook" | "form" => {
                let on = if mode == "form" {
                    WaitTrigger::Form
                } else {
                    let suffix = param_str(ctx, "options.webhookSuffix", 0, "")?;
                    WaitTrigger::Webhook {
                        http_method: param_str(ctx, "httpMethod", 0, "GET")?.to_uppercase(),
                        suffix: (!suffix.is_empty()).then_some(suffix),
                    }
                };
                WaitRequest {
                    resume_at: webhook_timeout(ctx, now)?,
                    on,
                }
            }
            other => {
                return Err(node_err(ctx, format!("Unknown Wait resume mode `{other}`")));
            }
        };

        if request.on == WaitTrigger::Time {
            let at = request.resume_at.unwrap_or(now);
            let wait = Duration::from_millis(u64::try_from((at - now).max(0)).unwrap_or(0));
            if wait <= ctx.hooks.max_inline_wait {
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
                return Ok(vec![port(ctx, 0).to_vec()]);
            }
        }
        if ctx.hooks.durable_wait {
            return Err(FlowError::Suspend(Box::new(request)));
        }
        Err(node_err(
            ctx,
            match request.on {
                WaitTrigger::Time => format!(
                    "Wait of {}s exceeds the in-process limit of {}s; the durable wait is not enabled (executor has no wait store)",
                    request.resume_at.unwrap_or(now).saturating_sub(now) / 1000,
                    ctx.hooks.max_inline_wait.as_secs()
                ),
                _ => format!(
                    "Wait resume mode `{mode}` needs the durable wait, which is not enabled (executor has no wait store)"
                ),
            },
        ))
    }
}

fn unit_ms(unit: &str) -> f64 {
    match unit {
        "seconds" => 1000.0,
        "minutes" => 60_000.0,
        "days" => 86_400_000.0,
        _ => 3_600_000.0,
    }
}

/// `limitWaitTime`: when a webhook/form wait gives up and continues on its own.
fn webhook_timeout(ctx: &NodeContext, now: i64) -> Result<Option<i64>, FlowError> {
    if !param_bool(ctx, "limitWaitTime", 0, false)? {
        return Ok(None);
    }
    if param_str(ctx, "limitType", 0, "afterTimeInterval")? == "atSpecifiedTime" {
        let v = param_or(ctx, "maxDateAndTime", 0, Value::Null)?;
        let tz = parse_zone(ctx.expr.timezone.as_deref().unwrap_or("UTC"));
        let at = parse_date(&v, tz)
            .ok_or_else(|| node_err(ctx, "Invalid date for 'Limit Wait Time'"))?;
        return Ok(Some(at.timestamp_millis()));
    }
    let amount = param_f64(ctx, "resumeAmount", 0, 1.0)?;
    let ms = amount * unit_ms(&param_str(ctx, "resumeUnit", 0, "hours")?);
    Ok(Some(now.saturating_add(ms.max(0.0) as i64)))
}

// ------------------------------------------------------------- Execute Workflow

#[derive(Debug)]
pub struct ExecuteWorkflowNode(NodeTypeDescription);

impl Default for ExecuteWorkflowNode {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecuteWorkflowNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Execute Workflow",
            "name": "n8n-nodes-base.executeWorkflow",
            "version": [1, 1.1, 1.2],
            "defaultVersion": 1.2,
            "description": "Execute another workflow",
            "group": ["transform"],
            "defaults": {"name": "Execute Workflow", "color": "#ff6d5a"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Source", "name": "source", "type": "options", "default": "database",
                 "options": [
                    {"name": "Database", "value": "database"},
                    {"name": "Parameter", "value": "parameter"}]},
                {"displayName": "Workflow", "name": "workflowId", "type": "workflowSelector", "default": "",
                 "displayOptions": {"show": {"source": ["database"]}}},
                {"displayName": "Workflow JSON", "name": "workflowJson", "type": "json", "default": "\n\n\n",
                 "displayOptions": {"show": {"source": ["parameter"]}}},
                {"displayName": "Mode", "name": "mode", "type": "options", "default": "once",
                 "options": [
                    {"name": "Run Once with All Items", "value": "once"},
                    {"name": "Run Once for Each Item", "value": "each"}]},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [{"displayName": "Wait For Sub-Workflow Completion", "name": "waitForSubWorkflow", "type": "boolean", "default": true}]}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for ExecuteWorkflowNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let Some(runner) = ctx.hooks.sub_workflow.clone() else {
            return Err(node_err(
                ctx,
                "Execute Workflow is not available in this runtime",
            ));
        };
        let items = port(ctx, 0);
        let (id, json) = match param_str(ctx, "source", 0, "database")?.as_str() {
            "parameter" => {
                let v = param_or(ctx, "workflowJson", 0, Value::Null)?;
                let v = match v {
                    Value::String(s) => serde_json::from_str(&s)
                        .map_err(|e| node_err(ctx, format!("Invalid workflow JSON: {e}")))?,
                    other => other,
                };
                (None, Some(v))
            }
            "database" => {
                let v = param_or(ctx, "workflowId", 0, Value::Null)?;
                // resource locator `{__rl, value, mode}` or a bare id
                let id = match &v {
                    Value::Object(o) => o.get("value").map(to_display),
                    Value::Null => None,
                    other => Some(to_display(other)),
                };
                match id {
                    Some(id) if !id.is_empty() => (Some(id), None),
                    _ => return Err(node_err(ctx, "No workflow selected")),
                }
            }
            other => {
                return Err(node_err(
                    ctx,
                    format!("Unsupported workflow source `{other}`"),
                ));
            }
        };
        let wait = param_bool(ctx, "options.waitForSubWorkflow", 0, true)?;
        let req = |items: Vec<Item>| SubWorkflowRequest {
            workflow_id: id.clone(),
            workflow_json: json.clone(),
            items,
        };
        let mut out = Vec::new();
        if param_str(ctx, "mode", 0, "once")? == "each" {
            for it in items {
                out.extend(runner.run(req(vec![it.clone()])).await?);
            }
        } else {
            out = runner.run(req(items.to_vec())).await?;
        }
        if !wait {
            // fire-and-forget callers continue with their own data
            return Ok(vec![items.to_vec()]);
        }
        Ok(vec![out])
    }
}

// ---------------------------------------------------------- Respond to Webhook

#[derive(Debug)]
pub struct RespondToWebhookNode(NodeTypeDescription);

impl Default for RespondToWebhookNode {
    fn default() -> Self {
        Self::new()
    }
}

impl RespondToWebhookNode {
    pub fn new() -> Self {
        Self(describe(json!({
            "displayName": "Respond to Webhook",
            "name": "n8n-nodes-base.respondToWebhook",
            "version": [1, 1.1, 1.2, 1.3],
            "defaultVersion": 1.3,
            "description": "Returns data for Webhook",
            "group": ["transform"],
            "defaults": {"name": "Respond to Webhook"},
            "inputs": ["main"],
            "outputs": ["main"],
            "properties": [
                {"displayName": "Respond With", "name": "respondWith", "type": "options", "default": "firstIncomingItem",
                 "options": [
                    {"name": "All Incoming Items", "value": "allIncomingItems"},
                    {"name": "Binary File", "value": "binary"},
                    {"name": "First Incoming Item", "value": "firstIncomingItem"},
                    {"name": "JSON", "value": "json"},
                    {"name": "No Data", "value": "noData"},
                    {"name": "Redirect", "value": "redirect"},
                    {"name": "Text", "value": "text"}]},
                {"displayName": "Response Body", "name": "responseBody", "type": "json", "default": "",
                 "displayOptions": {"show": {"respondWith": ["json", "text"]}}},
                {"displayName": "Redirect URL", "name": "redirectURL", "type": "string", "default": "",
                 "displayOptions": {"show": {"respondWith": ["redirect"]}}},
                {"displayName": "Response Data Source", "name": "inputFieldName", "type": "string", "default": "data",
                 "displayOptions": {"show": {"respondWith": ["binary"]}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [
                    {"displayName": "Response Code", "name": "responseCode", "type": "number", "default": 200},
                    {"displayName": "Response Headers", "name": "responseHeaders", "type": "fixedCollection", "default": {}},
                    {"displayName": "Put Response in Field", "name": "responseKey", "type": "string", "default": ""}]}
            ]
        })))
    }
}

#[async_trait]
impl FlowNode for RespondToWebhookNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let items = port(ctx, 0);
        let with = param_str(ctx, "respondWith", 0, "firstIncomingItem")?;
        let key = param_str(ctx, "options.responseKey", 0, "")?;
        let wrap = |v: Value| {
            if key.is_empty() {
                v
            } else {
                json!({ key.clone(): v })
            }
        };
        let default_code = if with == "redirect" { 307.0 } else { 200.0 };
        let status = param_f64(ctx, "options.responseCode", 0, default_code)? as u16;

        let body = match with.as_str() {
            "allIncomingItems" => wrap(Value::Array(
                items
                    .iter()
                    .map(|i| Value::Object(i.json.clone()))
                    .collect(),
            ))
            .into(),
            "firstIncomingItem" => items
                .first()
                .map(|i| wrap(Value::Object(i.json.clone())))
                .unwrap_or(Value::Null)
                .into(),
            "json" => {
                let v = param_or(ctx, "responseBody", 0, Value::Null)?;
                match v {
                    Value::String(s) => serde_json::from_str::<Value>(&s)
                        .map(wrap)
                        .map_err(|e| {
                            node_err(ctx, format!("Invalid JSON in 'Response Body': {e}"))
                        })?
                        .into(),
                    other => wrap(other).into(),
                }
            }
            "text" => ResponseBody::Text(param_str(ctx, "responseBody", 0, "")?),
            "redirect" => ResponseBody::Redirect(param_str(ctx, "redirectURL", 0, "")?),
            "noData" => ResponseBody::Empty,
            "binary" => {
                let prop = param_str(ctx, "inputFieldName", 0, "data")?;
                let bin = items
                    .first()
                    .and_then(|i| i.binary.as_ref())
                    .and_then(|b| b.get(&prop))
                    .ok_or_else(|| {
                        node_err(
                            ctx,
                            format!("No binary data `{prop}` on the first input item"),
                        )
                    })?;
                let data = base64::engine::general_purpose::STANDARD
                    .decode(bin.data.as_bytes())
                    .map_err(|e| node_err(ctx, format!("Binary data is not valid base64: {e}")))?;
                ResponseBody::Binary {
                    mime_type: bin.mime_type.clone(),
                    data,
                }
            }
            other => {
                return Err(node_err(
                    ctx,
                    format!("Respond With `{other}` is not supported"),
                ));
            }
        };

        let mut headers = Vec::new();
        for h in raw_array(ctx, "options.responseHeaders.entries") {
            let name = ctx.eval_param(h.get("name").unwrap_or(&Value::Null), 0)?;
            let value = ctx.eval_param(h.get("value").unwrap_or(&Value::Null), 0)?;
            let name = to_display(&name);
            if !name.is_empty() {
                headers.push((name, to_display(&value)));
            }
        }
        if let ResponseBody::Redirect(url) = &body {
            headers.push(("Location".into(), url.clone()));
        }

        if let Some(sink) = &ctx.hooks.response {
            sink.respond(WebhookResponse {
                status,
                headers,
                body,
            });
        }
        Ok(vec![items.to_vec()])
    }
}

impl From<Value> for ResponseBody {
    fn from(v: Value) -> Self {
        ResponseBody::Json(v)
    }
}
