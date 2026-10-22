//! RabbitMQ (`n8n-nodes-base.rabbitmq`): send messages to a queue or exchange, or take messages
//! off a queue. AMQP 0-9-1 through `lapin`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lapin::options::{BasicAckOptions, BasicGetOptions, BasicPublishOptions, QueueDeclareOptions};
use lapin::types::FieldTable;
use lapin::{BasicProperties, Connection, ConnectionProperties};
use serde_json::{Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop};
use crate::kit;

pub const NODE_TYPE: &str = "n8n-nodes-base.rabbitmq";
pub const CREDENTIAL: &str = "rabbitmq";

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "RabbitMQ")
        .property(
            Prop::string("hostname", "Host")
                .default("localhost")
                .required(),
        )
        .property(Prop::number("port", "Port").default(5672))
        .property(Prop::string("username", "User").default("guest"))
        .property(
            Prop::string("password", "Password")
                .password()
                .default("guest"),
        )
        .property(Prop::string("vhost", "Vhost").default("/"))
        .property(Prop::boolean("ssl", "SSL").description("amqps:// (usually port 5671)"))
}

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "RabbitMQ")
        .description("Send and receive messages with RabbitMQ")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:rabbit")
        .color("#FF6600")
        .credential(CREDENTIAL, true)
        .property(Prop::options(
            "operation",
            "Operation",
            vec![
                Choice::new("Send a Message to Queue", "sendMessage").action("Send a message"),
                Choice::new("Get Messages from Queue", "getMessages")
                    .action("Take messages off a queue"),
            ],
        ))
        .property(Prop::string("queue", "Queue / Routing Key").required())
        .property(
            Prop::string("exchange", "Exchange")
                .description("Empty: the default exchange (routes by queue name)")
                .show("operation", &["sendMessage"]),
        )
        .property(
            Prop::boolean("sendInputData", "Send Input Data")
                .default(true)
                .show("operation", &["sendMessage"]),
        )
        .property(
            Prop::string("message", "Message")
                .show("operation", &["sendMessage"])
                .show_bool("sendInputData", false),
        )
        .property(Prop::boolean("durable", "Durable Queue").default(true))
        .property(
            Prop::number("maxMessages", "Max Messages")
                .default(1)
                .show("operation", &["getMessages"]),
        )
        .property(
            Prop::boolean("ack", "Acknowledge")
                .default(true)
                .show("operation", &["getMessages"]),
        )
        .build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

fn uri(cred: &serde_json::Map<String, Value>) -> String {
    let get = |k: &str, d: &str| {
        Some(kit::cred_str(cred, k))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| d.to_owned())
    };
    let ssl = matches!(cred.get("ssl"), Some(Value::Bool(true)));
    let enc = |s: &str| {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                    char::from(b).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>()
    };
    format!(
        "{}://{}:{}@{}:{}/{}",
        if ssl { "amqps" } else { "amqp" },
        enc(&get("username", "guest")),
        enc(&get("password", "guest")),
        get("hostname", "localhost"),
        get("port", if ssl { "5671" } else { "5672" }),
        enc(&get("vhost", "/")),
    )
}

#[derive(Debug)]
struct RabbitNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

#[async_trait]
impl FlowNode for RabbitNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let name = ctx.node.name.clone();
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let e = |m: String| kit::err(&name, m);
        let conn = tokio::time::timeout(
            Duration::from_secs(15),
            Connection::connect(&uri(&cred), ConnectionProperties::default()),
        )
        .await
        .map_err(|_| e("connection timed out".into()))?
        .map_err(|x| e(format!("Could not connect: {x}")))?;
        let ch = conn.create_channel().await.map_err(|x| e(x.to_string()))?;
        let op = kit::str_param(ctx, &self.desc, "operation", 0)?;
        let queue = kit::str_param(ctx, &self.desc, "queue", 0)?;
        let durable = !matches!(ctx.eval_param_at("durable", 0)?, Some(Value::Bool(false)));
        let mut out = Vec::new();
        if op == "getMessages" {
            ch.queue_declare(
                &queue,
                QueueDeclareOptions {
                    durable,
                    ..QueueDeclareOptions::default()
                },
                FieldTable::default(),
            )
            .await
            .map_err(|x| e(x.to_string()))?;
            let max = ctx
                .eval_param_at("maxMessages", 0)?
                .and_then(|v| v.as_u64())
                .unwrap_or(1)
                .max(1);
            let ack = !matches!(ctx.eval_param_at("ack", 0)?, Some(Value::Bool(false)));
            for _ in 0..max {
                let Some(m) = ch
                    .basic_get(&queue, BasicGetOptions { no_ack: false })
                    .await
                    .map_err(|x| e(x.to_string()))?
                else {
                    break;
                };
                let text = String::from_utf8_lossy(&m.delivery.data).into_owned();
                let content = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
                out.push(Item::from_json(json!({
                    "content": content,
                    "exchange": m.delivery.exchange.as_str(),
                    "routingKey": m.delivery.routing_key.as_str(),
                    "redelivered": m.delivery.redelivered,
                })));
                if ack {
                    m.delivery
                        .ack(BasicAckOptions::default())
                        .await
                        .map_err(|x| e(x.to_string()))?;
                }
            }
        } else {
            let exchange = kit::str_param(ctx, &self.desc, "exchange", 0)?;
            if exchange.is_empty() {
                ch.queue_declare(
                    &queue,
                    QueueDeclareOptions {
                        durable,
                        ..QueueDeclareOptions::default()
                    },
                    FieldTable::default(),
                )
                .await
                .map_err(|x| e(x.to_string()))?;
            }
            for (i, item) in ctx.input.first().into_iter().flatten().enumerate() {
                let payload = if matches!(
                    ctx.eval_param_at("sendInputData", i)?,
                    Some(Value::Bool(false))
                ) {
                    kit::str_param(ctx, &self.desc, "message", i)?
                } else {
                    Value::Object(item.json.clone()).to_string()
                };
                ch.basic_publish(
                    &exchange,
                    &queue,
                    BasicPublishOptions::default(),
                    payload.as_bytes(),
                    BasicProperties::default(),
                )
                .await
                .map_err(|x| e(x.to_string()))?
                .await
                .map_err(|x| e(x.to_string()))?;
                out.push(item.clone());
            }
        }
        let _ = conn.close(200, "done").await;
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-rabbitmq`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(RabbitNode {
        desc: description(),
        creds: creds.clone(),
    }))
}
