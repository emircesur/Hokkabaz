//! Telegram (`n8n-nodes-base.telegram`) and Telegram Trigger (`n8n-nodes-base.telegramTrigger`).
//!
//! Bot API over HTTPS: `<baseUrl>/bot<token>/<method>`. The token lives in the URL, so
//! requests are sent through [`kit::call`], which reports failures without the URL.
//!
//! Node: message (sendMessage, editMessageText, deleteMessage, sendPhoto/Document/Video/
//! Audio/Animation from a URL, file id or an input binary), callback answerQuery, chat get,
//! file get (optionally downloads). Output is the Bot API response (`{ok, result}`), like n8n.
//!
//! Trigger: webhook mode is out of scope; it long-polls `getUpdates` through the polling
//! framework. The persisted cursor is the next `offset` (confirms what was delivered), the
//! `updates` parameter filters by update type, `chatIds` / `userIds` by sender. Unlike
//! listing-style pollers the first poll emits: Telegram queues updates for the bot, so they
//! are events, not history.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::item::BinaryData;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialTest, CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop, poll_times_prop};
use crate::kit::{self, BinFile, Multipart, err, node_err};
use crate::poll::{PollBatch, PollContext, PollRegistry, PolledItem, PollingTrigger};
use crate::template::display;

pub const NODE_TYPE: &str = "n8n-nodes-base.telegram";
pub const TRIGGER_TYPE: &str = "n8n-nodes-base.telegramTrigger";
pub const CREDENTIAL: &str = "telegramApi";

const ERROR_PATHS: &[&str] = &["description", "message", "error"];
/// Media operations: operation -> Bot API field carrying the file.
const MEDIA: &[(&str, &str)] = &[
    ("sendPhoto", "photo"),
    ("sendDocument", "document"),
    ("sendVideo", "video"),
    ("sendAudio", "audio"),
    ("sendAnimation", "animation"),
];
/// Bot API params that must be JSON numbers; parameter values arrive as strings.
const INT_FIELDS: &[&str] = &[
    "message_id",
    "message_thread_id",
    "reply_to_message_id",
    "cache_time",
    "duration",
    "width",
    "height",
];

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "Telegram API")
        .documentation("https://core.telegram.org/bots/api")
        .property(
            Prop::string("accessToken", "Access Token")
                .password()
                .required()
                .description("Bot token from @BotFather"),
        )
        .property(
            Prop::string("baseUrl", "Base URL")
                .default("https://api.telegram.org")
                .description("Bot API server, change only for a self-hosted Bot API"),
        )
        .test(CredentialTest::get(
            "={{$credentials.baseUrl}}/bot{{$credentials.accessToken}}",
            "/getMe",
        ))
}

fn is_media(op: &str) -> Option<&'static str> {
    MEDIA.iter().find(|(o, _)| *o == op).map(|(_, f)| *f)
}

fn description() -> NodeTypeDescription {
    let msg = |p: Prop| p.show("resource", &["message"]);
    let ops_with_chat = [
        "deleteMessage",
        "editMessageText",
        "sendMessage",
        "sendPhoto",
        "sendDocument",
        "sendVideo",
        "sendAudio",
        "sendAnimation",
    ];
    let media_ops: Vec<&str> = MEDIA.iter().map(|(o, _)| *o).collect();
    let markup_ops = [
        "sendMessage",
        "editMessageText",
        "sendPhoto",
        "sendDocument",
        "sendVideo",
        "sendAudio",
        "sendAnimation",
    ];

    let mut d = NodeBuilder::new(NODE_TYPE, "Telegram")
        .description("Send and edit messages and files through a Telegram bot")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:paper-plane")
        .color("#0088cc")
        .credential(CREDENTIAL, true)
        .property(
            Prop::options(
                "resource",
                "Resource",
                vec![
                    Choice::new("Callback", "callback"),
                    Choice::new("Chat", "chat"),
                    Choice::new("File", "file"),
                    Choice::new("Message", "message"),
                ],
            )
            .default("message"),
        )
        .property(
            Prop::options(
                "operation",
                "Operation",
                vec![Choice::new("Answer Query", "answerQuery")],
            )
            .show("resource", &["callback"]),
        )
        .property(
            Prop::options("operation", "Operation", vec![Choice::new("Get", "get")])
                .show("resource", &["chat"]),
        )
        .property(
            Prop::options("operation", "Operation", vec![Choice::new("Get", "get")])
                .show("resource", &["file"]),
        )
        .property(
            msg(Prop::options(
                "operation",
                "Operation",
                vec![
                    Choice::new("Delete Chat Message", "deleteMessage"),
                    Choice::new("Edit Message Text", "editMessageText"),
                    Choice::new("Send Animation", "sendAnimation"),
                    Choice::new("Send Audio", "sendAudio"),
                    Choice::new("Send Document", "sendDocument"),
                    Choice::new("Send Message", "sendMessage"),
                    Choice::new("Send Photo", "sendPhoto"),
                    Choice::new("Send Video", "sendVideo"),
                ],
            ))
            .default("sendMessage"),
        )
        .property(
            Prop::string("queryId", "Query ID")
                .required()
                .description("Unique identifier of the query to answer")
                .show("resource", &["callback"])
                .show("operation", &["answerQuery"]),
        )
        .property(
            Prop::string("chatId", "Chat ID")
                .required()
                .description(
                    "Unique identifier for the target chat or username of the target channel",
                )
                .show("resource", &["chat", "message"]),
        )
        .property(
            Prop::string("fileId", "File ID")
                .required()
                .show("resource", &["file"])
                .show("operation", &["get"]),
        )
        .property(
            Prop::boolean("download", "Download")
                .description("Whether to download the file into a binary property")
                .show("resource", &["file"])
                .show("operation", &["get"]),
        )
        .property(
            Prop::options(
                "messageType",
                "Message Type",
                vec![
                    Choice::new("Message", "message"),
                    Choice::new("Inline Message", "inlineMessage"),
                ],
            )
            .show("resource", &["message"])
            .show("operation", &["editMessageText"]),
        )
        .property(
            Prop::string("messageId", "Message ID")
                .required()
                .show("resource", &["message"])
                .show("operation", &["deleteMessage", "editMessageText"])
                .hide("messageType", &["inlineMessage"]),
        )
        .property(
            Prop::string("inlineMessageId", "Inline Message ID")
                .required()
                .show("resource", &["message"])
                .show("operation", &["editMessageText"])
                .show("messageType", &["inlineMessage"]),
        )
        .property(
            Prop::string("text", "Text")
                .required()
                .type_option("rows", 4)
                .show("resource", &["message"])
                .show("operation", &["sendMessage", "editMessageText"]),
        )
        .property(
            Prop::boolean("binaryData", "Binary File")
                .description("Whether the file comes from an input binary property")
                .show("resource", &["message"])
                .show("operation", &media_ops),
        )
        .property(
            Prop::string("binaryPropertyName", "Input Binary Field")
                .default("data")
                .show("resource", &["message"])
                .show("operation", &media_ops)
                .show_bool("binaryData", true),
        )
        .property(
            Prop::string("file", "File")
                .required()
                .description("URL or file_id of a file already on Telegram's servers")
                .show("resource", &["message"])
                .show("operation", &media_ops)
                .show_bool("binaryData", false),
        )
        .property(
            Prop::options(
                "replyMarkup",
                "Reply Markup",
                vec![
                    Choice::new("None", "none"),
                    Choice::new("Inline Keyboard", "inlineKeyboard"),
                    Choice::new("Reply Keyboard", "replyKeyboard"),
                    Choice::new("Reply Keyboard Remove", "replyKeyboardRemove"),
                    Choice::new("Force Reply", "forceReply"),
                ],
            )
            .show("resource", &["message"])
            .show("operation", &markup_ops),
        )
        .property(
            Prop::json("inlineKeyboard", "Inline Keyboard")
                .description("Rows of buttons: [[{\"text\": \"Go\", \"callback_data\": \"go\"}]]")
                .default("[]")
                .show("resource", &["message"])
                .show("replyMarkup", &["inlineKeyboard"]),
        )
        .property(
            Prop::json("replyKeyboard", "Reply Keyboard")
                .description("Rows of buttons: [[{\"text\": \"Yes\"}, {\"text\": \"No\"}]]")
                .default("[]")
                .show("resource", &["message"])
                .show("replyMarkup", &["replyKeyboard"]),
        )
        .property(
            Prop::collection(
                "additionalFields",
                "Additional Fields",
                vec![
                    Prop::string("caption", "Caption"),
                    Prop::options(
                        "parse_mode",
                        "Parse Mode",
                        vec![
                            Choice::new("HTML", "HTML"),
                            Choice::new("Markdown (Legacy)", "Markdown"),
                            Choice::new("MarkdownV2", "MarkdownV2"),
                        ],
                    ),
                    Prop::boolean("disable_notification", "Disable Notification"),
                    Prop::boolean("disable_web_page_preview", "Disable WebPage Preview"),
                    Prop::string("reply_to_message_id", "Reply To Message ID"),
                    Prop::string("message_thread_id", "Message Thread ID"),
                ],
            )
            .show("resource", &["message"])
            .show("operation", &ops_with_chat[1..]),
        )
        .property(
            Prop::collection(
                "additionalFields",
                "Additional Fields",
                vec![
                    Prop::number("cache_time", "Cache Time"),
                    Prop::boolean("show_alert", "Show Alert"),
                    Prop::string("text", "Text"),
                    Prop::string("url", "URL"),
                ],
            )
            .show("resource", &["callback"])
            .show("operation", &["answerQuery"]),
        )
        .build();
    kit::serve_versions(&mut d, &[1.0, 1.1, 1.2]);
    d
}

fn trigger_description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(TRIGGER_TYPE, "Telegram Trigger")
        .description("Starts the workflow on Telegram updates (long-polling getUpdates)")
        .icon("fa:paper-plane")
        .color("#0088cc")
        .polling_trigger()
        .credential(CREDENTIAL, true)
        .property(
            poll_times_prop()
                .default(json!({"item": [{"mode": "everyX", "value": 10, "unit": "seconds"}]})),
        )
        .property(Prop::collection(
            "additionalFields",
            "Additional Fields",
            vec![
                Prop::string("chatIds", "Restrict to Chat IDs").description(
                    "Comma-separated chat IDs; only updates from these chats start the workflow",
                ),
                Prop::string("userIds", "Restrict to User IDs").description(
                    "Comma-separated user IDs; only updates from these users start the workflow",
                ),
                Prop::number("timeout", "Long Poll Seconds")
                    .description("How long Telegram holds each getUpdates call open when idle")
                    .min(0.0)
                    .max(50.0),
            ],
        ))
        .build();
    let updates = kit::multi_options(
        "updates",
        "Trigger On",
        &[
            ("*", "*"),
            ("Callback Query", "callback_query"),
            ("Channel Post", "channel_post"),
            ("Edited Channel Post", "edited_channel_post"),
            ("Edited Message", "edited_message"),
            ("Inline Query", "inline_query"),
            ("Message", "message"),
            ("Poll", "poll"),
            ("Poll Answer", "poll_answer"),
            ("Pre-Checkout Query", "pre_checkout_query"),
            ("Shipping Query", "shipping_query"),
        ],
        json!(["message"]),
    );
    kit::insert_before(&mut d, "additionalFields", updates);
    kit::serve_versions(&mut d, &[1.0, 1.1]);
    d
}

// --------------------------------------------------------------------- api

fn api_url(cred: &Map<String, Value>, method: &str) -> String {
    format!(
        "{}/bot{}/{method}",
        kit::cred_str(cred, "baseUrl").trim_end_matches('/'),
        kit::cred_str(cred, "accessToken")
    )
}

/// The Bot API wraps failures as `{ok: false, description}`; HTTP status carries them too.
fn check_ok(node: &str, body: Value) -> Result<Value, FlowError> {
    if body.get("ok") == Some(&Value::Bool(false)) {
        let d = body.get("description").map(display).unwrap_or_default();
        return Err(err(node, format!("Telegram returned an error: {d}")));
    }
    Ok(body)
}

fn int_if_numeric(key: &str, v: Value) -> Value {
    if INT_FIELDS.contains(&key)
        && let Value::String(s) = &v
        && let Ok(n) = s.trim().parse::<i64>()
    {
        return json!(n);
    }
    v
}

fn empty(v: &Value) -> bool {
    matches!(v, Value::Null) || matches!(v, Value::String(s) if s.is_empty())
}

/// Button rows from either Telegram's shape (`[[{text}]]`, possibly as a JSON string) or
/// n8n's (`{rows: [{row: {buttons: [{text, additionalFields}]}}]}`).
fn keyboard_rows(v: &Value) -> Vec<Value> {
    let v = match v {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        other => other.clone(),
    };
    if let Value::Array(a) = &v {
        return a.clone();
    }
    let rows = v
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    rows.iter()
        .map(|r| {
            let buttons = r
                .pointer("/row/buttons")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Value::Array(
                buttons
                    .into_iter()
                    .map(|b| {
                        let mut out = Map::new();
                        if let Some(t) = b.get("text") {
                            out.insert("text".into(), t.clone());
                        }
                        if let Some(Value::Object(extra)) = b.get("additionalFields") {
                            out.extend(
                                extra
                                    .iter()
                                    .filter(|(_, v)| !empty(v))
                                    .map(|(k, v)| (k.clone(), v.clone())),
                            );
                        }
                        Value::Object(out)
                    })
                    .collect(),
            )
        })
        .collect()
}

#[derive(Debug)]
struct TelegramNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

/// One Bot API request: JSON body, or multipart when a file is uploaded.
struct Call {
    method: &'static str,
    fields: Map<String, Value>,
    upload: Option<(&'static str, BinFile)>,
}

impl TelegramNode {
    fn reply_markup(&self, ctx: &NodeContext, i: usize) -> Result<Option<Value>, FlowError> {
        let kind = kit::str_param(ctx, &self.desc, "replyMarkup", i)?;
        Ok(match kind.as_str() {
            "inlineKeyboard" => {
                let rows = kit::param(ctx, &self.desc, "inlineKeyboard", i)?.unwrap_or(Value::Null);
                Some(json!({ "inline_keyboard": keyboard_rows(&rows) }))
            }
            "replyKeyboard" => {
                let rows = kit::param(ctx, &self.desc, "replyKeyboard", i)?.unwrap_or(Value::Null);
                let mut m = json!({ "keyboard": keyboard_rows(&rows) });
                if let Some(Value::Object(o)) = kit::field(ctx, "replyKeyboardOptions", i)?
                    && let Value::Object(mm) = &mut m
                {
                    mm.extend(o);
                }
                Some(m)
            }
            "replyKeyboardRemove" => Some(json!({ "remove_keyboard": true })),
            "forceReply" => Some(json!({ "force_reply": true })),
            _ => None,
        })
    }

    fn additional(
        &self,
        ctx: &NodeContext,
        i: usize,
        fields: &mut Map<String, Value>,
    ) -> Result<(), FlowError> {
        if let Some(Value::Object(extra)) = kit::field(ctx, "additionalFields", i)? {
            for (k, v) in extra {
                // n8n-only knob, not a Bot API parameter
                if k != "appendAttribution" && !empty(&v) {
                    let v = int_if_numeric(&k, v);
                    fields.insert(k, v);
                }
            }
        }
        Ok(())
    }

    fn build(&self, ctx: &NodeContext, i: usize) -> Result<Call, FlowError> {
        let resource = kit::str_param(ctx, &self.desc, "resource", i)?;
        let operation = kit::str_param(ctx, &self.desc, "operation", i)?;
        let s = |name: &str| kit::str_param(ctx, &self.desc, name, i);
        let mut fields = Map::new();
        let method: &'static str = match (resource.as_str(), operation.as_str()) {
            ("callback", "answerQuery") => {
                fields.insert("callback_query_id".into(), s("queryId")?.into());
                self.additional(ctx, i, &mut fields)?;
                "answerCallbackQuery"
            }
            ("chat", "get") => {
                fields.insert("chat_id".into(), s("chatId")?.into());
                "getChat"
            }
            ("file", "get") => {
                fields.insert("file_id".into(), s("fileId")?.into());
                "getFile"
            }
            ("message", "deleteMessage") => {
                fields.insert("chat_id".into(), s("chatId")?.into());
                fields.insert(
                    "message_id".into(),
                    int_if_numeric("message_id", s("messageId")?.into()),
                );
                "deleteMessage"
            }
            ("message", "editMessageText") => {
                if s("messageType")? == "inlineMessage" {
                    fields.insert("inline_message_id".into(), s("inlineMessageId")?.into());
                } else {
                    fields.insert("chat_id".into(), s("chatId")?.into());
                    fields.insert(
                        "message_id".into(),
                        int_if_numeric("message_id", s("messageId")?.into()),
                    );
                }
                fields.insert("text".into(), s("text")?.into());
                self.additional(ctx, i, &mut fields)?;
                // Bot API only accepts an inline keyboard when editing
                if s("replyMarkup")? == "inlineKeyboard"
                    && let Some(m) = self.reply_markup(ctx, i)?
                {
                    fields.insert("reply_markup".into(), m);
                }
                "editMessageText"
            }
            ("message", "sendMessage") => {
                fields.insert("chat_id".into(), s("chatId")?.into());
                fields.insert("text".into(), s("text")?.into());
                self.additional(ctx, i, &mut fields)?;
                if let Some(m) = self.reply_markup(ctx, i)? {
                    fields.insert("reply_markup".into(), m);
                }
                "sendMessage"
            }
            ("message", op) if is_media(op).is_some() => {
                let field = is_media(op).unwrap_or("document");
                fields.insert("chat_id".into(), s("chatId")?.into());
                self.additional(ctx, i, &mut fields)?;
                if let Some(m) = self.reply_markup(ctx, i)? {
                    fields.insert("reply_markup".into(), m);
                }
                let method = MEDIA
                    .iter()
                    .find(|(o, _)| *o == op)
                    .map_or("sendDocument", |(o, _)| *o);
                let mut upload = None;
                if kit::bool_param(ctx, &self.desc, "binaryData", i)? {
                    let prop = s("binaryPropertyName")?;
                    let item = ctx
                        .input
                        .first()
                        .and_then(|v| v.get(i))
                        .ok_or_else(|| node_err(ctx, "No input item"))?;
                    upload = Some((field, kit::binary_file(ctx, item, &prop)?));
                } else {
                    fields.insert(field.into(), s("file")?.into());
                }
                return Ok(Call {
                    method,
                    fields,
                    upload,
                });
            }
            (r, o) => return Err(node_err(ctx, format!("Unsupported operation `{r}`/`{o}`"))),
        };
        Ok(Call {
            method,
            fields,
            upload: None,
        })
    }

    async fn send(
        &self,
        ctx: &NodeContext,
        http: &reqwest::Client,
        cred: &Map<String, Value>,
        call: Call,
    ) -> Result<Value, FlowError> {
        let url = api_url(cred, call.method);
        let rb = http.post(url);
        let rb = match call.upload {
            None => kit::json_body(rb, &Value::Object(call.fields)),
            Some((field, file)) => {
                let mut mp = Multipart::new();
                for (k, v) in &call.fields {
                    match v {
                        Value::String(s) => mp.text(k, s),
                        other => mp.text(k, &other.to_string()),
                    }
                }
                mp.file(field, &file);
                let (ct, body) = mp.finish();
                rb.header("content-type", ct).body(body)
            }
        };
        let body = kit::call(&ctx.node.name, rb, ERROR_PATHS).await?;
        check_ok(&ctx.node.name, body)
    }

    async fn run_item(
        &self,
        ctx: &NodeContext,
        http: &reqwest::Client,
        cred: &Map<String, Value>,
        i: usize,
    ) -> Result<Vec<Item>, FlowError> {
        let call = self.build(ctx, i)?;
        let is_file_get = call.method == "getFile";
        let body = self.send(ctx, http, cred, call).await?;
        let mut item = kit::to_item(body.clone());
        if is_file_get && kit::bool_param(ctx, &self.desc, "download", i)? {
            let path = body
                .pointer("/result/file_path")
                .and_then(Value::as_str)
                .ok_or_else(|| node_err(ctx, "Telegram did not return a file path"))?;
            let url = format!(
                "{}/file/bot{}/{path}",
                kit::cred_str(cred, "baseUrl").trim_end_matches('/'),
                kit::cred_str(cred, "accessToken")
            );
            let res =
                http.get(url).send().await.map_err(|e| {
                    node_err(ctx, format!("The download failed: {}", e.without_url()))
                })?;
            if !res.status().is_success() {
                return Err(node_err(
                    ctx,
                    format!("The download failed: {}", res.status().as_u16()),
                ));
            }
            let mime = res
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_owned();
            let bytes = res
                .bytes()
                .await
                .map_err(|e| node_err(ctx, format!("The download failed: {}", e.without_url())))?;
            let name = path.rsplit('/').next().unwrap_or(path).to_owned();
            let mut bin = indexmap::IndexMap::new();
            bin.insert(
                "data".to_owned(),
                BinaryData {
                    data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                    mime_type: mime,
                    file_name: Some(name),
                    ..BinaryData::default()
                },
            );
            item.binary = Some(bin);
        }
        Ok(vec![item])
    }
}

#[async_trait]
impl FlowNode for TelegramNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let input_len = ctx.input.first().map_or(0, Vec::len);
        if input_len == 0 {
            return Ok(vec![Vec::new()]);
        }
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let http = kit::client(&ctx.node.name, Duration::from_secs(60))?;
        let mut out = Vec::new();
        for i in 0..input_len {
            let res = self.run_item(ctx, &http, &cred, i).await;
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

// ----------------------------------------------------------------- trigger

#[derive(Debug)]
struct TelegramTrigger {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

fn id_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The update's payload object (`message`, `callback_query`, ...), and its type key.
fn payload(update: &Value) -> Option<(&str, &Value)> {
    update
        .as_object()?
        .iter()
        .find(|(k, _)| k.as_str() != "update_id")
        .map(|(k, v)| (k.as_str(), v))
}

fn update_passes(update: &Value, types: &[String], chats: &[String], users: &[String]) -> bool {
    let Some((kind, body)) = payload(update) else {
        return false;
    };
    if !types.is_empty() && !types.iter().any(|t| t == "*" || t == kind) {
        return false;
    }
    let ids = |v: Option<&Value>| {
        v.and_then(|x| {
            x.as_i64()
                .map(|n| n.to_string())
                .or_else(|| x.as_str().map(str::to_owned))
        })
    };
    if !chats.is_empty() {
        let chat = ids(body.pointer("/chat/id")).or_else(|| ids(body.pointer("/message/chat/id")));
        if !chat.is_some_and(|c| chats.contains(&c)) {
            return false;
        }
    }
    if !users.is_empty() && !ids(body.pointer("/from/id")).is_some_and(|u| users.contains(&u)) {
        return false;
    }
    true
}

#[async_trait]
impl PollingTrigger for TelegramTrigger {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    fn emit_on_first_poll(&self) -> bool {
        true
    }

    async fn poll(&self, cx: &PollContext, cursor: Option<&Value>) -> Result<PollBatch, FlowError> {
        let node = cx.node().name.clone();
        let raw = cx.credential(CREDENTIAL).await?;
        let cred = self.creds.with_defaults(CREDENTIAL, &raw);

        let types: Vec<String> = match cx.param("updates")? {
            Some(Value::Array(a)) => a.iter().map(display).collect(),
            Some(Value::String(s)) if !s.is_empty() => id_list(&s),
            _ => vec!["message".to_owned()],
        };
        let chats = id_list(&cx.param_str("additionalFields.chatIds", "")?);
        let users = id_list(&cx.param_str("additionalFields.userIds", "")?);
        let timeout = cx
            .param("additionalFields.timeout")?
            .and_then(|v| v.as_u64().or_else(|| display(&v).parse().ok()))
            .unwrap_or(0)
            .min(50);

        let mut body = json!({ "timeout": timeout });
        if let Some(o) = cursor.and_then(Value::as_i64) {
            body["offset"] = json!(o);
        }
        if !types.iter().any(|t| t == "*") {
            body["allowed_updates"] = json!(types);
        }
        let http = kit::client(&node, Duration::from_secs(timeout + 20))?;
        let res = kit::call(
            &node,
            kit::json_body(http.post(api_url(&cred, "getUpdates")), &body),
            ERROR_PATHS,
        )
        .await?;
        let res = check_ok(&node, res)?;
        let updates = res
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let next = updates
            .iter()
            .filter_map(|u| u.get("update_id").and_then(Value::as_i64))
            .max();
        let items = updates
            .into_iter()
            .filter(|u| update_passes(u, &types, &chats, &users))
            .filter_map(|u| {
                let id = u.get("update_id").map(display)?;
                Some(PolledItem::new(id, u))
            })
            .collect();
        // filtered-out updates are still confirmed, or Telegram would resend them forever
        Ok(PollBatch {
            items,
            cursor: next.map(|n| json!(n + 1)),
        })
    }
}

/// Node, trigger and credential type into the shared registries. Features: `int-telegram`.
pub fn register(
    nodes: &mut NodeRegistry,
    creds: &CredentialTypes,
    polls: &mut PollRegistry,
) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(TelegramNode {
        desc: description(),
        creds: creds.clone(),
    }))?;
    polls.register(
        nodes,
        Arc::new(TelegramTrigger {
            desc: trigger_description(),
            creds: creds.clone(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_shapes() {
        let tg = json!([[{"text": "a", "callback_data": "x"}]]);
        assert_eq!(keyboard_rows(&tg), tg.as_array().unwrap().clone());
        assert_eq!(
            keyboard_rows(&json!(tg.to_string())),
            tg.as_array().unwrap().clone()
        );
        let n8n = json!({"rows": [{"row": {"buttons": [
            {"text": "go", "additionalFields": {"callback_data": "g", "url": ""}}]}}]});
        assert_eq!(
            keyboard_rows(&n8n),
            vec![json!([{"text": "go", "callback_data": "g"}])]
        );
    }

    #[test]
    fn update_filters() {
        let u = json!({"update_id": 1, "message": {"chat": {"id": -100}, "from": {"id": 7}}});
        let t = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert!(update_passes(&u, &t(&["message"]), &[], &[]));
        assert!(update_passes(&u, &t(&["*"]), &[], &[]));
        assert!(!update_passes(&u, &t(&["edited_message"]), &[], &[]));
        assert!(update_passes(
            &u,
            &t(&["message"]),
            &t(&["-100"]),
            &t(&["7"])
        ));
        assert!(!update_passes(&u, &t(&["message"]), &t(&["5"]), &[]));
        assert!(!update_passes(&u, &t(&["message"]), &[], &t(&["8"])));
        let cb = json!({"update_id": 2, "callback_query": {"from": {"id": 7}, "message": {"chat": {"id": 3}}}});
        assert!(update_passes(
            &cb,
            &t(&["callback_query"]),
            &t(&["3"]),
            &t(&["7"])
        ));
    }
}
