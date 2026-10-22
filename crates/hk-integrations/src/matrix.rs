//! Matrix (`n8n-nodes-base.matrix`) over the client-server REST API v3.
//!
//! message (create, getAll), room (create, join, leave, invite, kick), event (get).
//! Auth is the access token as a bearer header. Room ids carry `!` and `:`; they are put in
//! the path through `Url::path_segments_mut`, which encodes each segment.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{Authenticate, CredentialTest, CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop, return_all_and_limit};
use crate::kit::{self, node_err};

pub const NODE_TYPE: &str = "n8n-nodes-base.matrix";
pub const CREDENTIAL: &str = "matrixApi";

const ERROR_PATHS: &[&str] = &["error", "errcode"];
/// Server-side cap of `/messages` per page.
const PAGE: usize = 100;

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "Matrix API")
        .documentation("https://spec.matrix.org/latest/client-server-api/")
        .property(
            Prop::string("homeserverUrl", "Homeserver URL")
                .default("https://matrix-client.matrix.org")
                .required(),
        )
        .property(
            Prop::string("accessToken", "Access Token")
                .password()
                .required(),
        )
        .authenticate(Authenticate::bearer("={{$credentials.accessToken}}"))
        .test(CredentialTest::get(
            "={{$credentials.homeserverUrl}}",
            "/_matrix/client/v3/account/whoami",
        ))
}

fn description() -> NodeTypeDescription {
    let on = |p: Prop, res: &str, ops: &[&str]| p.show("resource", &[res]).show("operation", ops);
    let mut b = NodeBuilder::new(NODE_TYPE, "Matrix")
        .description("Send messages and manage rooms on a Matrix homeserver")
        .subtitle("={{ $parameter.operation + \": \" + $parameter.resource }}")
        .icon("fa:comments")
        .color("#0dbd8b")
        .credential(CREDENTIAL, true)
        .property(
            Prop::options(
                "resource",
                "Resource",
                vec![
                    Choice::new("Event", "event"),
                    Choice::new("Message", "message"),
                    Choice::new("Room", "room"),
                ],
            )
            .default("message"),
        )
        .property(
            Prop::options("operation", "Operation", vec![Choice::new("Get", "get")])
                .show("resource", &["event"]),
        )
        .property(
            Prop::options(
                "operation",
                "Operation",
                vec![
                    Choice::new("Send", "create"),
                    Choice::new("Get Many", "getAll"),
                ],
            )
            .show("resource", &["message"]),
        )
        .property(
            Prop::options(
                "operation",
                "Operation",
                vec![
                    Choice::new("Create", "create"),
                    Choice::new("Invite", "invite"),
                    Choice::new("Join", "join"),
                    Choice::new("Kick", "kick"),
                    Choice::new("Leave", "leave"),
                ],
            )
            .show("resource", &["room"]),
        )
        .property(
            Prop::string("roomId", "Room ID")
                .required()
                .placeholder("!123abc:matrix.org")
                .show("resource", &["event", "message"]),
        )
        .property(
            on(
                Prop::string("roomId", "Room ID").required(),
                "room",
                &["invite", "kick", "leave"],
            )
            .placeholder("!123abc:matrix.org"),
        )
        .property(on(
            Prop::string("eventId", "Event ID").required(),
            "event",
            &["get"],
        ))
        .property(on(
            Prop::string("text", "Text")
                .required()
                .type_option("rows", 3),
            "message",
            &["create"],
        ))
        .property(on(
            Prop::options(
                "messageType",
                "Message Type",
                vec![
                    Choice::new("Text", "m.text"),
                    Choice::new("Emote", "m.emote"),
                    Choice::new("Notice", "m.notice"),
                ],
            ),
            "message",
            &["create"],
        ))
        .property(on(
            Prop::options(
                "messageFormat",
                "Message Format",
                vec![
                    Choice::new("Plain Text", "plain"),
                    Choice::new("HTML", "org.matrix.custom.html"),
                ],
            ),
            "message",
            &["create"],
        ))
        .property(
            on(
                Prop::string("fallbackText", "Fallback Text")
                    .description("Plain text shown by clients that cannot render HTML"),
                "message",
                &["create"],
            )
            .show("messageFormat", &["org.matrix.custom.html"]),
        )
        .property(on(
            Prop::string("roomName", "Room Name"),
            "room",
            &["create"],
        ))
        .property(on(
            Prop::options(
                "preset",
                "Preset",
                vec![
                    Choice::new("Private Chat", "private_chat"),
                    Choice::new("Public Chat", "public_chat"),
                    Choice::new("Trusted Private Chat", "trusted_private_chat"),
                ],
            ),
            "room",
            &["create"],
        ))
        .property(on(
            Prop::string("roomAlias", "Room Alias")
                .description("Local part, without `#` and server"),
            "room",
            &["create"],
        ))
        .property(on(
            Prop::string("roomIdOrAlias", "Room ID or Alias")
                .required()
                .placeholder("#room:matrix.org"),
            "room",
            &["join"],
        ))
        .property(on(
            Prop::string("userId", "User ID")
                .required()
                .placeholder("@alice:matrix.org"),
            "room",
            &["invite", "kick"],
        ))
        .property(on(Prop::string("reason", "Reason"), "room", &["kick"]));
    b = b.properties(return_all_and_limit(Some("message"), &["getAll"]));
    let mut d = b.build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

// ---------------------------------------------------------------- request

#[derive(Debug)]
struct Req {
    method: reqwest::Method,
    segments: Vec<String>,
    query: Vec<(String, String)>,
    body: Option<Value>,
}

impl Req {
    fn new(method: reqwest::Method, segments: &[&str]) -> Self {
        Self {
            method,
            segments: segments.iter().map(|s| (*s).to_owned()).collect(),
            query: Vec::new(),
            body: None,
        }
    }
}

fn txn_id() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("hk{nanos:x}{}", N.fetch_add(1, Ordering::Relaxed))
}

fn url(node: &str, homeserver: &str, segments: &[String]) -> Result<reqwest::Url, FlowError> {
    let all: Vec<String> = ["_matrix", "client", "v3"]
        .iter()
        .map(|s| (*s).to_owned())
        .chain(segments.iter().cloned())
        .collect();
    kit::join_url(node, homeserver, &all)
}

#[derive(Debug)]
struct MatrixNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

impl MatrixNode {
    fn plan(&self, ctx: &NodeContext, i: usize) -> Result<Req, FlowError> {
        let s = |name: &str| kit::str_param(ctx, &self.desc, name, i);
        let resource = s("resource")?;
        let operation = s("operation")?;
        let need = |name: &str, label: &str| -> Result<String, FlowError> {
            let v = s(name)?;
            if v.is_empty() {
                Err(node_err(ctx, format!("Parameter '{label}' is required")))
            } else {
                Ok(v)
            }
        };
        use reqwest::Method as M;
        Ok(match (resource.as_str(), operation.as_str()) {
            ("message", "create") => {
                let room = need("roomId", "Room ID")?;
                let txn = txn_id();
                let mut r = Req::new(M::PUT, &["rooms", &room, "send", "m.room.message", &txn]);
                let text = s("text")?;
                let msgtype = Some(s("messageType")?)
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "m.text".into());
                let mut body = json!({ "msgtype": msgtype });
                if s("messageFormat")? == "org.matrix.custom.html" {
                    let fallback = Some(s("fallbackText")?)
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| text.clone());
                    body["body"] = fallback.into();
                    body["format"] = "org.matrix.custom.html".into();
                    body["formatted_body"] = text.into();
                } else {
                    body["body"] = text.into();
                }
                r.body = Some(body);
                r
            }
            ("message", "getAll") => {
                let room = need("roomId", "Room ID")?;
                let mut r = Req::new(M::GET, &["rooms", &room, "messages"]);
                r.query.push(("dir".into(), "b".into()));
                r
            }
            ("room", "create") => {
                let mut r = Req::new(M::POST, &["createRoom"]);
                let mut body = Map::new();
                for (key, param) in [
                    ("name", "roomName"),
                    ("preset", "preset"),
                    ("room_alias_name", "roomAlias"),
                ] {
                    let v = s(param)?;
                    if !v.is_empty() {
                        body.insert(key.into(), v.into());
                    }
                }
                r.body = Some(Value::Object(body));
                r
            }
            ("room", "join") => {
                let target = need("roomIdOrAlias", "Room ID or Alias")?;
                let mut r = Req::new(M::POST, &["join", &target]);
                r.body = Some(json!({}));
                r
            }
            ("room", "leave") => {
                let room = need("roomId", "Room ID")?;
                let mut r = Req::new(M::POST, &["rooms", &room, "leave"]);
                r.body = Some(json!({}));
                r
            }
            ("room", "invite") => {
                let room = need("roomId", "Room ID")?;
                let mut r = Req::new(M::POST, &["rooms", &room, "invite"]);
                r.body = Some(json!({ "user_id": need("userId", "User ID")? }));
                r
            }
            ("room", "kick") => {
                let room = need("roomId", "Room ID")?;
                let mut r = Req::new(M::POST, &["rooms", &room, "kick"]);
                let mut body = json!({ "user_id": need("userId", "User ID")? });
                let reason = s("reason")?;
                if !reason.is_empty() {
                    body["reason"] = reason.into();
                }
                r.body = Some(body);
                r
            }
            ("event", "get") => {
                let room = need("roomId", "Room ID")?;
                let event = need("eventId", "Event ID")?;
                Req::new(M::GET, &["rooms", &room, "event", &event])
            }
            (r, o) => return Err(node_err(ctx, format!("Unsupported operation `{r}`/`{o}`"))),
        })
    }

    async fn send(
        &self,
        ctx: &NodeContext,
        http: &reqwest::Client,
        base: &str,
        token: &str,
        req: &Req,
        extra_query: &[(String, String)],
    ) -> Result<Value, FlowError> {
        let u = url(&ctx.node.name, base, &req.segments)?;
        let mut rb = http
            .request(req.method.clone(), u)
            .bearer_auth(token)
            .query(&req.query)
            .query(extra_query);
        if let Some(b) = &req.body {
            rb = kit::json_body(rb, b);
        }
        kit::call(&ctx.node.name, rb, ERROR_PATHS).await
    }

    async fn run_item(
        &self,
        ctx: &NodeContext,
        http: &reqwest::Client,
        cred: &Map<String, Value>,
        i: usize,
    ) -> Result<Vec<Item>, FlowError> {
        let base = kit::cred_str(cred, "homeserverUrl");
        let token = kit::cred_str(cred, "accessToken");
        let req = self.plan(ctx, i)?;
        let is_list = req.segments.last().map(String::as_str) == Some("messages");
        if !is_list {
            let body = self.send(ctx, http, &base, &token, &req, &[]).await?;
            return Ok(vec![match body {
                Value::Object(m) if m.is_empty() => Item::from_json(json!({ "success": true })),
                other => kit::to_item(other),
            }]);
        }

        let all = kit::bool_param(ctx, &self.desc, "returnAll", i)?;
        let limit = if all {
            usize::MAX
        } else {
            kit::param(ctx, &self.desc, "limit", i)?
                .and_then(|v| v.as_u64())
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0)
                .unwrap_or(50)
        };
        let mut out: Vec<Item> = Vec::new();
        let mut from: Option<String> = None;
        loop {
            let size = limit.saturating_sub(out.len()).min(PAGE);
            let mut q = vec![("limit".to_owned(), size.to_string())];
            if let Some(f) = &from {
                q.push(("from".into(), f.clone()));
            }
            let body = self.send(ctx, http, &base, &token, &req, &q).await?;
            let chunk = body
                .get("chunk")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let n = chunk.len();
            out.extend(chunk.into_iter().map(kit::to_item));
            from = body.get("end").and_then(Value::as_str).map(str::to_owned);
            if out.len() >= limit || n == 0 || from.is_none() {
                break;
            }
        }
        out.truncate(limit);
        Ok(out)
    }
}

#[async_trait]
impl FlowNode for MatrixNode {
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

/// Node and credential type into the shared registries. Features: `int-matrix`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(MatrixNode {
        desc: description(),
        creds: creds.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_ids_are_encoded_per_segment() {
        let u = url(
            "n",
            "https://hs.example/base/",
            &["rooms".into(), "!a b:x/y".into(), "leave".into()],
        )
        .unwrap();
        assert_eq!(u.path(), "/base/_matrix/client/v3/rooms/!a%20b:x%2Fy/leave");
        assert!(url("n", "ftp://x", &[]).is_err());
    }
}
