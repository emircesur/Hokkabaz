//! Gotify (`n8n-nodes-base.gotify`): message create / delete / getAll.
//!
//! Gotify has two token kinds sent as `X-Gotify-Key`: an application token publishes, a
//! client token reads and deletes. The credential holds both; each operation picks its own
//! (which is why this is a hand-written node and not a `RestNode`, whose auth is per node).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop, return_all_and_limit};
use crate::kit::{self, node_err};

pub const NODE_TYPE: &str = "n8n-nodes-base.gotify";
pub const CREDENTIAL: &str = "gotifyApi";

const ERROR_PATHS: &[&str] = &["errorDescription", "error"];
/// Gotify caps `limit` at 200.
const PAGE: usize = 200;

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "Gotify API")
        .documentation("https://gotify.net/docs/")
        .property(
            Prop::string("url", "URL")
                .required()
                .placeholder("https://gotify.example.com"),
        )
        .property(
            Prop::string("appApiToken", "App API Token")
                .password()
                .description("Application token, needed to create messages"),
        )
        .property(
            Prop::string("clientApiToken", "Client API Token")
                .password()
                .description("Client token, needed to list and delete messages"),
        )
}

fn description() -> NodeTypeDescription {
    let mut b = NodeBuilder::new(NODE_TYPE, "Gotify")
        .description("Consume the Gotify push notification API")
        .subtitle("={{ $parameter.operation + \": \" + $parameter.resource }}")
        .icon("fa:bell")
        .color("#71c6e8")
        .credential(CREDENTIAL, true)
        .property(Prop::options(
            "resource",
            "Resource",
            vec![Choice::new("Message", "message")],
        ))
        .property(
            Prop::options(
                "operation",
                "Operation",
                vec![
                    Choice::new("Create", "create"),
                    Choice::new("Delete", "delete"),
                    Choice::new("Get Many", "getAll"),
                ],
            )
            .show("resource", &["message"]),
        )
        .property(
            Prop::string("message", "Message")
                .required()
                .type_option("rows", 3)
                .show("resource", &["message"])
                .show("operation", &["create"]),
        )
        .property(
            Prop::number("priority", "Priority")
                .default(1)
                .min(0.0)
                .show("resource", &["message"])
                .show("operation", &["create"]),
        )
        .property(
            Prop::collection(
                "additionalFields",
                "Additional Fields",
                vec![
                    Prop::options(
                        "contentType",
                        "Content Type",
                        vec![
                            Choice::new("Plain", "text/plain"),
                            Choice::new("Markdown", "text/markdown"),
                        ],
                    ),
                    Prop::string("title", "Title"),
                ],
            )
            .show("resource", &["message"])
            .show("operation", &["create"]),
        )
        .property(
            Prop::string("messageId", "Message ID")
                .required()
                .show("resource", &["message"])
                .show("operation", &["delete"]),
        );
    b = b.properties(return_all_and_limit(Some("message"), &["getAll"]));
    let mut d = b.build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

#[derive(Debug)]
struct GotifyNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

impl GotifyNode {
    async fn run_item(
        &self,
        ctx: &NodeContext,
        http: &reqwest::Client,
        cred: &Map<String, Value>,
        i: usize,
    ) -> Result<Vec<Item>, FlowError> {
        let node = &ctx.node.name;
        let base = kit::cred_str(cred, "url");
        let token = |key: &str, label: &str| -> Result<String, FlowError> {
            let t = kit::cred_str(cred, key);
            if t.is_empty() {
                Err(node_err(
                    ctx,
                    format!("The credential has no {label}, which this operation needs"),
                ))
            } else {
                Ok(t)
            }
        };
        let operation = kit::str_param(ctx, &self.desc, "operation", i)?;
        match operation.as_str() {
            "create" => {
                let app = token("appApiToken", "App API Token")?;
                let mut body = json!({
                    "message": kit::str_param(ctx, &self.desc, "message", i)?,
                    "priority": kit::param(ctx, &self.desc, "priority", i)?
                        .and_then(|v| v.as_i64().or_else(|| crate::template::display(&v).parse().ok()))
                        .unwrap_or(1),
                });
                if let Some(t) = kit::field_str(ctx, "additionalFields.title", i)? {
                    body["title"] = t.into();
                }
                if let Some(ct) = kit::field_str(ctx, "additionalFields.contentType", i)? {
                    body["extras"] = json!({ "client::display": { "contentType": ct } });
                }
                let url = kit::join_url(node, &base, &["message".into()])?;
                let rb = kit::json_body(http.post(url).header("X-Gotify-Key", app), &body);
                Ok(vec![kit::to_item(kit::call(node, rb, ERROR_PATHS).await?)])
            }
            "delete" => {
                let client = token("clientApiToken", "Client API Token")?;
                let id = kit::str_param(ctx, &self.desc, "messageId", i)?;
                if id.is_empty() {
                    return Err(node_err(ctx, "Parameter 'Message ID' is required"));
                }
                let url = kit::join_url(node, &base, &["message".into(), id])?;
                let rb = http.delete(url).header("X-Gotify-Key", client);
                kit::call(node, rb, ERROR_PATHS).await?;
                Ok(vec![Item::from_json(json!({ "success": true }))])
            }
            "getAll" => {
                let client = token("clientApiToken", "Client API Token")?;
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
                let url = kit::join_url(node, &base, &["message".into()])?;
                let mut out: Vec<Item> = Vec::new();
                let mut since: Option<i64> = None;
                loop {
                    let size = limit.saturating_sub(out.len()).min(PAGE);
                    let mut rb = http
                        .get(url.clone())
                        .header("X-Gotify-Key", &client)
                        .query(&[("limit", size.to_string())]);
                    if let Some(s) = since {
                        rb = rb.query(&[("since", s.to_string())]);
                    }
                    let body = kit::call(node, rb, ERROR_PATHS).await?;
                    let msgs = body
                        .get("messages")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let n = msgs.len();
                    out.extend(msgs.into_iter().map(kit::to_item));
                    // paging.next is only present while older messages remain
                    let next = body
                        .pointer("/paging/next")
                        .is_some()
                        .then(|| body.pointer("/paging/since").and_then(Value::as_i64))
                        .flatten();
                    match next {
                        Some(s) if out.len() < limit && n > 0 => since = Some(s),
                        _ => break,
                    }
                }
                out.truncate(limit);
                Ok(out)
            }
            other => Err(node_err(ctx, format!("Unsupported operation `{other}`"))),
        }
    }
}

#[async_trait]
impl FlowNode for GotifyNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let input_len = ctx.input.first().map_or(0, Vec::len);
        if input_len == 0 {
            return Ok(vec![Vec::new()]);
        }
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let http = kit::client(&ctx.node.name, Duration::from_secs(30))?;
        let mut out = Vec::new();
        for i in 0..input_len {
            let res = self.run_item(ctx, &http, &cred, i).await;
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-gotify`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(GotifyNode {
        desc: description(),
        creds: creds.clone(),
    }))
}
