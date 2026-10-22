//! Slack (`n8n-nodes-base.slack` v2.x): the common operations over the Web API with a bot
//! token. Parameter names follow n8n so imported workflows run unchanged: `resource`,
//! `operation`, `select`, `channelId` (a resource locator or a plain id), `text`,
//! `otherOptions`.
//!
//! Slack answers HTTP 200 with `{ok: false, error}` on failures; that is mapped to a node error.
//! Not covered: files, stars, user groups, OAuth2 credentials.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{Authenticate, CredentialTest, CredentialType, CredentialTypes};
use crate::describe::Prop;
use crate::kit;

pub const NODE_TYPE: &str = "n8n-nodes-base.slack";
pub const CREDENTIAL: &str = "slackApi";
const API: &str = "https://slack.com/api";

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "Slack API")
        .documentation("https://api.slack.com/authentication/token-types")
        .property(
            Prop::string("accessToken", "Access Token")
                .password()
                .required()
                .description("Bot token (xoxb-...) or user token"),
        )
        .property(Prop::string("signatureSecret", "Signature Secret").password())
        .property(
            Prop::string("apiUrl", "API URL")
                .default(API)
                .description("Only change for a Slack-compatible server"),
        )
        .authenticate(Authenticate::header(
            "Authorization",
            "=Bearer {{$credentials.accessToken}}",
        ))
        .test(CredentialTest::get(API, "/auth.test"))
}

fn description() -> NodeTypeDescription {
    let d = json!({
        "displayName": "Slack",
        "name": NODE_TYPE,
        "version": [1, 2, 2.1, 2.2, 2.3],
        "defaultVersion": 2.3,
        "description": "Consume Slack API",
        "subtitle": "={{ $parameter[\"operation\"] + \": \" + $parameter[\"resource\"] }}",
        "group": ["output"],
        "defaults": {"name": "Slack", "color": "#4A154B"},
        "inputs": ["main"],
        "outputs": ["main"],
        "credentials": [{"name": CREDENTIAL, "required": true}],
        "properties": [
            {"displayName": "Resource", "name": "resource", "type": "options", "default": "message",
             "options": [{"name": "Channel", "value": "channel"}, {"name": "Message", "value": "message"},
                         {"name": "Reaction", "value": "reaction"}, {"name": "User", "value": "user"}]},
            {"displayName": "Operation", "name": "operation", "type": "options", "default": "post",
             "displayOptions": {"show": {"resource": ["message"]}},
             "options": [{"name": "Delete", "value": "delete"}, {"name": "Get Permalink", "value": "getPermalink"},
                         {"name": "Send", "value": "post", "action": "Send a message"}, {"name": "Update", "value": "update"}]},
            {"displayName": "Operation", "name": "operation", "type": "options", "default": "getAll",
             "displayOptions": {"show": {"resource": ["channel"]}},
             "options": [{"name": "Create", "value": "create"}, {"name": "Get", "value": "get"},
                         {"name": "Get Many", "value": "getAll"}, {"name": "History", "value": "history"},
                         {"name": "Join", "value": "join"}]},
            {"displayName": "Operation", "name": "operation", "type": "options", "default": "getAll",
             "displayOptions": {"show": {"resource": ["user"]}},
             "options": [{"name": "Get", "value": "info"}, {"name": "Get Many", "value": "getAll"}]},
            {"displayName": "Operation", "name": "operation", "type": "options", "default": "add",
             "displayOptions": {"show": {"resource": ["reaction"]}},
             "options": [{"name": "Add", "value": "add"}, {"name": "Remove", "value": "remove"}]},
            {"displayName": "Send Message To", "name": "select", "type": "options", "default": "channel",
             "displayOptions": {"show": {"resource": ["message"], "operation": ["post"]}},
             "options": [{"name": "Channel", "value": "channel"}, {"name": "User", "value": "user"}]},
            {"displayName": "Channel", "name": "channelId", "type": "string", "default": "",
             "description": "Channel id (or #name). A resource locator object works too.",
             "displayOptions": {"show": {"resource": ["message", "channel", "reaction"]}}},
            {"displayName": "User", "name": "user", "type": "string", "default": "",
             "displayOptions": {"show": {"resource": ["message"], "operation": ["post"], "select": ["user"]}}},
            {"displayName": "User ID", "name": "userId", "type": "string", "default": "",
             "displayOptions": {"show": {"resource": ["user"], "operation": ["info"]}}},
            {"displayName": "Message Type", "name": "messageType", "type": "options", "default": "text",
             "displayOptions": {"show": {"resource": ["message"], "operation": ["post", "update"]}},
             "options": [{"name": "Simple Text Message", "value": "text"}, {"name": "Blocks", "value": "block"}]},
            {"displayName": "Message Text", "name": "text", "type": "string", "default": "",
             "displayOptions": {"show": {"resource": ["message"], "operation": ["post", "update"]}}},
            {"displayName": "Blocks", "name": "blocksUi", "type": "json", "default": "",
             "displayOptions": {"show": {"resource": ["message"], "operation": ["post", "update"], "messageType": ["block"]}}},
            {"displayName": "Message Timestamp", "name": "ts", "type": "string", "default": "",
             "displayOptions": {"show": {"resource": ["message", "reaction"], "operation": ["update", "delete", "getPermalink", "add", "remove"]}}},
            {"displayName": "Channel Name", "name": "channelName", "type": "string", "default": "",
             "displayOptions": {"show": {"resource": ["channel"], "operation": ["create"]}}},
            {"displayName": "Emoji Code", "name": "name", "type": "string", "default": "",
             "displayOptions": {"show": {"resource": ["reaction"]}}},
            {"displayName": "Return All", "name": "returnAll", "type": "boolean", "default": false,
             "displayOptions": {"show": {"resource": ["channel", "user"], "operation": ["getAll", "history"]}}},
            {"displayName": "Limit", "name": "limit", "type": "number", "default": 50,
             "displayOptions": {"show": {"resource": ["channel", "user"], "operation": ["getAll", "history"], "returnAll": [false]}}},
            {"displayName": "Other Options", "name": "otherOptions", "type": "collection", "default": {}, "placeholder": "Add option",
             "displayOptions": {"show": {"resource": ["message"], "operation": ["post", "update"]}},
             "options": [
                {"displayName": "Reply to a Message", "name": "thread_ts", "type": "string", "default": ""},
                {"displayName": "Include Link to Workflow", "name": "includeLinkToWorkflow", "type": "boolean", "default": true},
                {"displayName": "Unfurl Links", "name": "unfurl_links", "type": "boolean", "default": false},
                {"displayName": "Unfurl Media", "name": "unfurl_media", "type": "boolean", "default": false},
                {"displayName": "Send as User", "name": "username", "type": "string", "default": ""},
                {"displayName": "Icon Emoji", "name": "icon_emoji", "type": "string", "default": ""}]}
        ]
    });
    serde_json::from_value(d).expect("static slack description")
}

#[derive(Debug)]
struct SlackNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

/// A plain string, or the `value` of a resource locator (`{__rl: true, mode, value}`).
fn locator(v: &Value) -> String {
    match v {
        Value::Object(m) => m
            .get("value")
            .map(crate::template::display)
            .unwrap_or_default(),
        other => crate::template::display(other),
    }
}

fn s(
    ctx: &NodeContext,
    desc: &NodeTypeDescription,
    name: &str,
    i: usize,
) -> Result<String, FlowError> {
    Ok(kit::param(ctx, desc, name, i)?
        .map(|v| locator(&v))
        .unwrap_or_default())
}

/// The Web API method and JSON/form payload for one item.
fn plan(
    ctx: &NodeContext,
    d: &NodeTypeDescription,
    i: usize,
) -> Result<(&'static str, Map<String, Value>, Option<&'static str>), FlowError> {
    let resource = s(ctx, d, "resource", i)?;
    let op = s(ctx, d, "operation", i)?;
    let mut body = Map::new();
    let put = |m: &mut Map<String, Value>, k: &str, v: String| {
        if !v.is_empty() {
            m.insert(k.to_owned(), Value::String(v));
        }
    };
    let message_body = |m: &mut Map<String, Value>| -> Result<(), FlowError> {
        if s(ctx, d, "messageType", i)? == "block" {
            let raw = kit::param(ctx, d, "blocksUi", i)?.unwrap_or(Value::Null);
            let blocks = match raw {
                Value::String(t) => serde_json::from_str::<Value>(&t)
                    .map_err(|e| kit::node_err(ctx, format!("Blocks is not valid JSON: {e}")))?,
                other => other,
            };
            // `{blocks: [...]}` (Block Kit Builder export) or a bare array
            m.insert(
                "blocks".into(),
                blocks.get("blocks").cloned().unwrap_or(blocks),
            );
        }
        put(m, "text", s(ctx, d, "text", i)?);
        for k in ["unfurl_links", "unfurl_media"] {
            if let Some(Value::Bool(b)) = kit::field(ctx, &format!("otherOptions.{k}"), i)? {
                m.insert(k.into(), Value::Bool(b));
            }
        }
        for k in ["thread_ts", "username", "icon_emoji"] {
            if let Some(v) = kit::field_str(ctx, &format!("otherOptions.{k}"), i)? {
                m.insert(k.into(), Value::String(v));
            }
        }
        Ok(())
    };
    Ok(match (resource.as_str(), op.as_str()) {
        ("message", "post") => {
            let target = if s(ctx, d, "select", i)? == "user" {
                s(ctx, d, "user", i)?
            } else {
                s(ctx, d, "channelId", i)?
            };
            if target.is_empty() {
                return Err(kit::node_err(ctx, "Specify the channel or user to send to"));
            }
            put(&mut body, "channel", target);
            message_body(&mut body)?;
            ("chat.postMessage", body, None)
        }
        ("message", "update") => {
            put(&mut body, "channel", s(ctx, d, "channelId", i)?);
            put(&mut body, "ts", s(ctx, d, "ts", i)?);
            message_body(&mut body)?;
            ("chat.update", body, None)
        }
        ("message", "delete") => {
            put(&mut body, "channel", s(ctx, d, "channelId", i)?);
            put(&mut body, "ts", s(ctx, d, "ts", i)?);
            ("chat.delete", body, None)
        }
        ("message", "getPermalink") => {
            put(&mut body, "channel", s(ctx, d, "channelId", i)?);
            put(&mut body, "message_ts", s(ctx, d, "ts", i)?);
            ("chat.getPermalink", body, None)
        }
        ("channel", "create") => {
            put(&mut body, "name", s(ctx, d, "channelName", i)?);
            ("conversations.create", body, None)
        }
        ("channel", "get") => {
            put(&mut body, "channel", s(ctx, d, "channelId", i)?);
            ("conversations.info", body, Some("channel"))
        }
        ("channel", "join") => {
            put(&mut body, "channel", s(ctx, d, "channelId", i)?);
            ("conversations.join", body, Some("channel"))
        }
        ("channel", "getAll") => ("conversations.list", body, Some("channels")),
        ("channel", "history") => {
            put(&mut body, "channel", s(ctx, d, "channelId", i)?);
            ("conversations.history", body, Some("messages"))
        }
        ("user", "info") => {
            put(&mut body, "user", s(ctx, d, "userId", i)?);
            ("users.info", body, Some("user"))
        }
        ("user", "getAll") => ("users.list", body, Some("members")),
        ("reaction", "add" | "remove") => {
            put(&mut body, "channel", s(ctx, d, "channelId", i)?);
            put(&mut body, "timestamp", s(ctx, d, "ts", i)?);
            put(
                &mut body,
                "name",
                s(ctx, d, "name", i)?.trim_matches(':').to_owned(),
            );
            (
                if op == "add" {
                    "reactions.add"
                } else {
                    "reactions.remove"
                },
                body,
                None,
            )
        }
        (r, o) => {
            return Err(kit::node_err(
                ctx,
                format!("Slack {r} › {o} is not supported"),
            ));
        }
    })
}

fn is_list(method: &str) -> bool {
    matches!(
        method,
        "conversations.list" | "users.list" | "conversations.history"
    )
}

impl SlackNode {
    async fn run_item(
        &self,
        ctx: &NodeContext,
        http: &reqwest::Client,
        token: &str,
        api: &str,
        i: usize,
    ) -> Result<Vec<hk_flow::Item>, FlowError> {
        let node = &ctx.node.name;
        let (method, body, out_key) = plan(ctx, &self.desc, i)?;
        let return_all = is_list(method) && kit::bool_param(ctx, &self.desc, "returnAll", i)?;
        let limit = kit::param(ctx, &self.desc, "limit", i)?
            .and_then(|v| v.as_u64())
            .unwrap_or(50) as usize;
        let mut items: Vec<Value> = Vec::new();
        let mut single = Value::Null;
        let mut cursor = String::new();
        for _ in 0..200 {
            let mut payload = body.clone();
            if is_list(method) {
                payload.insert(
                    "limit".into(),
                    json!(if return_all { 200 } else { limit.clamp(1, 200) }),
                );
                if !cursor.is_empty() {
                    payload.insert("cursor".into(), Value::String(cursor.clone()));
                }
            }
            let rb = http
                .post(format!("{}/{method}", api.trim_end_matches('/')))
                .header("authorization", format!("Bearer {token}"));
            let resp = kit::call(
                node,
                kit::json_body(rb, &Value::Object(payload)),
                &["error", "message"],
            )
            .await?;
            if resp.get("ok") == Some(&Value::Bool(false)) {
                let e = resp
                    .get("error")
                    .map(crate::template::display)
                    .unwrap_or_default();
                let hint = match e.as_str() {
                    "not_in_channel" => " (invite the bot to the channel first)",
                    "channel_not_found" => {
                        " (check the channel id; private channels need the bot invited)"
                    }
                    "invalid_auth" | "not_authed" | "token_revoked" => " (check the credentials)",
                    "missing_scope" => {
                        " (the token lacks a scope: see needed/provided in the Slack app settings)"
                    }
                    _ => "",
                };
                return Err(kit::err(
                    node,
                    format!("Slack returned an error: {e}{hint}"),
                ));
            }
            match out_key {
                Some(k) => match resp.get(k) {
                    Some(Value::Array(a)) => items.extend(a.iter().cloned()),
                    Some(v) => single = v.clone(),
                    None => {}
                },
                None => single = resp.clone(),
            }
            let next = resp
                .pointer("/response_metadata/next_cursor")
                .map(crate::template::display)
                .unwrap_or_default();
            if !is_list(method) || next.is_empty() || (!return_all && items.len() >= limit) {
                break;
            }
            cursor = next;
        }
        if is_list(method) {
            if !return_all {
                items.truncate(limit);
            }
            return Ok(items.into_iter().map(kit::to_item).collect());
        }
        Ok(vec![kit::to_item(single)])
    }
}

#[async_trait]
impl FlowNode for SlackNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let n = ctx.input.first().map_or(0, Vec::len);
        if n == 0 {
            return Ok(vec![Vec::new()]);
        }
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let token = kit::cred_str(&cred, "accessToken");
        let api = kit::cred_str(&cred, "apiUrl");
        let http = kit::client(&ctx.node.name, Duration::from_secs(60))?;
        let mut out = Vec::new();
        for i in 0..n {
            let res = self.run_item(ctx, &http, &token, &api, i).await;
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(SlackNode {
        desc: description(),
        creds: creds.clone(),
    }))
}
