//! Trigger node descriptions. A trigger is the start of an execution: the executor does
//! not run it, it emits the items supplied by the trigger runtime (`triggers/`). `execute`
//! is the pass-through used when something runs a trigger like an ordinary node (manual
//! "execute node", pinned test data).

use async_trait::async_trait;
use serde_json::{Value, json};

use super::util::describe;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::Item;
use crate::node::{FlowNode, NodeContext, NodeOutput};
use crate::triggers::{
    APP_EVENT_TYPE, CHAT_TYPE, ERROR_TRIGGER_TYPE, EXECUTE_WORKFLOW_TRIGGER_TYPE, FILE_WATCH_TYPE,
    FORM_TYPE, MANUAL_TYPE, SCHEDULE_TYPE, WEBHOOK_TYPE,
};

#[derive(Debug)]
pub struct TriggerNode(NodeTypeDescription);

#[async_trait]
impl FlowNode for TriggerNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let items = ctx.input.first().cloned().unwrap_or_default();
        Ok(vec![if items.is_empty() {
            vec![Item::default()]
        } else {
            items
        }])
    }
}

fn trigger(name: &str, display: &str, versions: Value, desc: &str, props: Value) -> TriggerNode {
    let default_version = versions
        .as_array()
        .and_then(|a| a.last())
        .cloned()
        .unwrap_or(json!(1));
    TriggerNode(describe(json!({
        "displayName": display,
        "name": name,
        "version": versions,
        "defaultVersion": default_version,
        "description": desc,
        "group": ["trigger"],
        "defaults": {"name": display, "color": "#909298"},
        "inputs": [],
        "outputs": ["main"],
        "properties": props,
    })))
}

fn interval_values() -> Value {
    json!([
        {"displayName": "Trigger Interval", "name": "field", "type": "options", "default": "days",
         "options": [
            {"name": "Seconds", "value": "seconds"}, {"name": "Minutes", "value": "minutes"},
            {"name": "Hours", "value": "hours"}, {"name": "Days", "value": "days"},
            {"name": "Weeks", "value": "weeks"}, {"name": "Months", "value": "months"},
            {"name": "Custom (Cron)", "value": "cronExpression"}]},
        {"displayName": "Seconds Between Triggers", "name": "secondsInterval", "type": "number", "default": 30,
         "displayOptions": {"show": {"field": ["seconds"]}}},
        {"displayName": "Minutes Between Triggers", "name": "minutesInterval", "type": "number", "default": 10,
         "displayOptions": {"show": {"field": ["minutes"]}}},
        {"displayName": "Hours Between Triggers", "name": "hoursInterval", "type": "number", "default": 1,
         "displayOptions": {"show": {"field": ["hours"]}}},
        {"displayName": "Days Between Triggers", "name": "daysInterval", "type": "number", "default": 1,
         "displayOptions": {"show": {"field": ["days"]}}},
        {"displayName": "Weeks Between Triggers", "name": "weeksInterval", "type": "number", "default": 1,
         "displayOptions": {"show": {"field": ["weeks"]}}},
        {"displayName": "Months Between Triggers", "name": "monthsInterval", "type": "number", "default": 1,
         "displayOptions": {"show": {"field": ["months"]}}},
        {"displayName": "Trigger on Weekdays", "name": "triggerAtDay", "type": "multiOptions", "default": [0],
         "displayOptions": {"show": {"field": ["weeks"]}},
         "options": [
            {"name": "Sunday", "value": 0}, {"name": "Monday", "value": 1}, {"name": "Tuesday", "value": 2},
            {"name": "Wednesday", "value": 3}, {"name": "Thursday", "value": 4},
            {"name": "Friday", "value": 5}, {"name": "Saturday", "value": 6}]},
        {"displayName": "Trigger at Day of Month", "name": "triggerAtDayOfMonth", "type": "number", "default": 1,
         "displayOptions": {"show": {"field": ["months"]}}},
        {"displayName": "Trigger at Hour", "name": "triggerAtHour", "type": "number", "default": 0,
         "displayOptions": {"show": {"field": ["days", "weeks", "months"]}}},
        {"displayName": "Trigger at Minute", "name": "triggerAtMinute", "type": "number", "default": 0,
         "displayOptions": {"show": {"field": ["hours", "days", "weeks", "months"]}}},
        {"displayName": "Expression", "name": "expression", "type": "string", "default": "",
         "placeholder": "eg. 0 15 * * 1-5",
         "displayOptions": {"show": {"field": ["cronExpression"]}}}
    ])
}

pub fn trigger_nodes() -> Vec<TriggerNode> {
    vec![
        trigger(
            MANUAL_TYPE,
            "Manual Trigger",
            json!([1]),
            "Runs the flow on clicking a button in the editor",
            json!([]),
        ),
        trigger(
            SCHEDULE_TYPE,
            "Schedule Trigger",
            json!([1, 1.1, 1.2]),
            "Triggers the workflow on a given schedule",
            json!([
                {"displayName": "Trigger Rules", "name": "rule", "type": "fixedCollection",
                 "typeOptions": {"multipleValues": true}, "default": {"interval": [{"field": "days"}]},
                 "options": [{"name": "interval", "displayName": "Trigger Interval", "values": interval_values()}]}
            ]),
        ),
        trigger(
            WEBHOOK_TYPE,
            "Webhook",
            json!([1, 1.1, 2, 2.1]),
            "Starts the workflow when a webhook is called",
            json!([
                {"displayName": "HTTP Method", "name": "httpMethod", "type": "options", "default": "GET",
                 "options": [
                    {"name": "DELETE", "value": "DELETE"}, {"name": "GET", "value": "GET"},
                    {"name": "HEAD", "value": "HEAD"}, {"name": "PATCH", "value": "PATCH"},
                    {"name": "POST", "value": "POST"}, {"name": "PUT", "value": "PUT"}]},
                {"displayName": "Path", "name": "path", "type": "string", "default": "",
                 "placeholder": "webhook"},
                {"displayName": "Authentication", "name": "authentication", "type": "options", "default": "none",
                 "options": [
                    {"name": "None", "value": "none"}, {"name": "Basic Auth", "value": "basicAuth"},
                    {"name": "Header Auth", "value": "headerAuth"}, {"name": "JWT Auth", "value": "jwtAuth"}]},
                {"displayName": "Respond", "name": "responseMode", "type": "options", "default": "onReceived",
                 "options": [
                    {"name": "Immediately", "value": "onReceived"},
                    {"name": "When Last Node Finishes", "value": "lastNode"},
                    {"name": "Using 'Respond to Webhook' Node", "value": "responseNode"}]},
                {"displayName": "Response Data", "name": "responseData", "type": "options", "default": "firstEntryJson",
                 "displayOptions": {"show": {"responseMode": ["lastNode"]}},
                 "options": [
                    {"name": "All Entries", "value": "allEntries"},
                    {"name": "First Entry JSON", "value": "firstEntryJson"},
                    {"name": "First Entry Binary", "value": "firstEntryBinary"},
                    {"name": "No Response Body", "value": "noData"}]},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "options": [
                    {"displayName": "Response Code", "name": "responseCode", "type": "number", "default": 200},
                    {"displayName": "Raw Body", "name": "rawBody", "type": "boolean", "default": false},
                    {"displayName": "Response Data", "name": "responseData", "type": "string", "default": ""},
                    {"displayName": "Property Name", "name": "responseBinaryPropertyName", "type": "string", "default": "data"}]}
            ]),
        ),
        trigger(
            FORM_TYPE,
            "n8n Form Trigger",
            json!([1, 2, 2.1]),
            "Generate webforms in n8n and pass their responses to the workflow",
            json!([
                {"displayName": "Form Title", "name": "formTitle", "type": "string", "default": ""},
                {"displayName": "Form Fields", "name": "formFields", "type": "fixedCollection",
                 "typeOptions": {"multipleValues": true}, "default": {"values": [{"fieldLabel": ""}]}}
            ]),
        ),
        trigger(
            CHAT_TYPE,
            "Chat Trigger",
            json!([1, 1.1, 1.2]),
            "Runs the workflow when an n8n generated webchat is submitted",
            json!([
                {"displayName": "Options", "name": "options", "type": "collection", "default": {}}
            ]),
        ),
        trigger(
            EXECUTE_WORKFLOW_TRIGGER_TYPE,
            "Execute Workflow Trigger",
            json!([1, 1.1]),
            "Helpers for calling other workflows",
            json!([
                {"displayName": "Input Data Mode", "name": "inputSource", "type": "options", "default": "passthrough",
                 "options": [
                    {"name": "Define Using Fields Below", "value": "workflowInputs"},
                    {"name": "Define Using JSON Example", "value": "jsonExample"},
                    {"name": "Accept All Data", "value": "passthrough"}]}
            ]),
        ),
        trigger(
            ERROR_TRIGGER_TYPE,
            "Error Trigger",
            json!([1]),
            "Triggers the workflow when another workflow has an error",
            json!([]),
        ),
        trigger(
            APP_EVENT_TYPE,
            "App Event Trigger",
            json!([1]),
            "Starts the workflow when an event is published inside Hokkabaz",
            json!([
                {"displayName": "Event", "name": "event", "type": "string", "default": "",
                 "placeholder": "session/finished", "description": "Event name; a trailing * matches a prefix"}
            ]),
        ),
        trigger(
            FILE_WATCH_TYPE,
            "File Watch Trigger",
            json!([1]),
            "Starts the workflow when a file or folder changes (desktop only)",
            json!([
                {"displayName": "Path", "name": "path", "type": "string", "default": ""},
                {"displayName": "Recursive", "name": "recursive", "type": "boolean", "default": false},
                {"displayName": "Events", "name": "events", "type": "multiOptions", "default": ["create", "modify"],
                 "options": [
                    {"name": "Created", "value": "create"}, {"name": "Modified", "value": "modify"},
                    {"name": "Deleted", "value": "delete"}]}
            ]),
        ),
    ]
}
