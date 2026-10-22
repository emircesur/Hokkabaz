//! MQTT (`n8n-nodes-base.mqtt`): publish a message, or listen on topics for a while and return
//! what arrived. (The listening trigger of n8n is a long-lived connection; here "Receive"
//! collects messages for up to `timeoutSeconds` / `maxMessages` per run, which fits schedules.)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS, Transport};
use serde_json::{Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop};
use crate::kit;

pub const NODE_TYPE: &str = "n8n-nodes-base.mqtt";
pub const CREDENTIAL: &str = "mqtt";

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "MQTT")
        .property(Prop::string("host", "Host").default("localhost").required())
        .property(Prop::number("port", "Port").default(1883))
        .property(Prop::string("username", "Username"))
        .property(Prop::string("password", "Password").password())
        .property(Prop::string("clientId", "Client ID").description("Empty: a random one"))
        .property(Prop::boolean("ssl", "SSL").description("Connect over TLS (usually port 8883)"))
}

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "MQTT")
        .description("Publish to or read from an MQTT broker")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:satellite-dish")
        .color("#660066")
        .credential(CREDENTIAL, true)
        .property(Prop::options(
            "operation",
            "Operation",
            vec![
                Choice::new("Publish", "publish").action("Publish a message"),
                Choice::new("Receive", "receive").action("Collect messages from topics"),
            ],
        ))
        .property(
            Prop::string("topic", "Topic")
                .required()
                .show("operation", &["publish"]),
        )
        .property(
            Prop::boolean("sendInputData", "Send Input Data")
                .default(true)
                .show("operation", &["publish"]),
        )
        .property(
            Prop::string("message", "Message")
                .show("operation", &["publish"])
                .show_bool("sendInputData", false),
        )
        .property(
            Prop::string("topics", "Topics")
                .description("Comma separated, wildcards (+ #) allowed")
                .required()
                .show("operation", &["receive"]),
        )
        .property(
            Prop::number("maxMessages", "Max Messages")
                .default(1)
                .show("operation", &["receive"]),
        )
        .property(
            Prop::number("timeoutSeconds", "Wait (seconds)")
                .default(10)
                .show("operation", &["receive"]),
        )
        .property(Prop::collection(
            "options",
            "Options",
            vec![
                Prop::options(
                    "qos",
                    "QoS",
                    vec![
                        Choice::new("At most once", 0),
                        Choice::new("At least once", 1),
                        Choice::new("Exactly once", 2),
                    ],
                )
                .default(0),
                Prop::boolean("retain", "Retain"),
            ],
        ))
        .build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

fn qos(n: i64) -> QoS {
    match n {
        1 => QoS::AtLeastOnce,
        2 => QoS::ExactlyOnce,
        _ => QoS::AtMostOnce,
    }
}

fn options(cred: &serde_json::Map<String, Value>) -> MqttOptions {
    let host = Some(kit::cred_str(cred, "host"))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".into());
    let ssl = matches!(cred.get("ssl"), Some(Value::Bool(true)));
    let port = kit::cred_str(cred, "port")
        .parse::<u16>()
        .unwrap_or(if ssl { 8883 } else { 1883 });
    let id = Some(kit::cred_str(cred, "clientId"))
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| format!("hokkabaz-{:x}", std::process::id() ^ (port as u32) << 8));
    let mut o = MqttOptions::new(id, host, port);
    o.set_keep_alive(Duration::from_secs(20));
    let user = kit::cred_str(cred, "username");
    if !user.is_empty() {
        o.set_credentials(user, kit::cred_str(cred, "password"));
    }
    if ssl {
        o.set_transport(Transport::tls_with_default_config());
    }
    o
}

#[derive(Debug)]
struct MqttNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

#[async_trait]
impl FlowNode for MqttNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let name = ctx.node.name.clone();
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let op = kit::str_param(ctx, &self.desc, "operation", 0)?;
        let (client, mut events) = AsyncClient::new(options(&cred), 32);
        let mut out = Vec::new();
        if op == "receive" {
            let topics = kit::str_param(ctx, &self.desc, "topics", 0)?;
            let max = ctx
                .eval_param_at("maxMessages", 0)?
                .and_then(|v| v.as_u64())
                .unwrap_or(1)
                .max(1);
            let wait = ctx
                .eval_param_at("timeoutSeconds", 0)?
                .and_then(|v| v.as_u64())
                .unwrap_or(10)
                .clamp(1, 600);
            let q = ctx
                .eval_param_at("options.qos", 0)?
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
            let mut subscribed = false;
            loop {
                match tokio::time::timeout_at(deadline, events.poll()).await {
                    Err(_) => break,
                    Ok(Err(e)) => {
                        return Err(kit::err(&name, format!("MQTT connection failed: {e}")));
                    }
                    Ok(Ok(Event::Incoming(Packet::ConnAck(_)))) if !subscribed => {
                        for t in topics.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                            client
                                .subscribe(t, qos(q))
                                .await
                                .map_err(|e| kit::err(&name, e.to_string()))?;
                        }
                        subscribed = true;
                    }
                    Ok(Ok(Event::Incoming(Packet::Publish(p)))) => {
                        let text = String::from_utf8_lossy(&p.payload).into_owned();
                        let message =
                            serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
                        out.push(Item::from_json(json!({"topic": p.topic, "message": message, "qos": p.qos as u8, "retain": p.retain})));
                        if out.len() as u64 >= max {
                            break;
                        }
                    }
                    Ok(Ok(_)) => {}
                }
            }
            let _ = client.disconnect().await;
            return Ok(vec![out]);
        }
        // publish: one message per input item
        let input_len = ctx.input.first().map_or(0, Vec::len);
        let q = ctx
            .eval_param_at("options.qos", 0)?
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let retain = matches!(
            ctx.eval_param_at("options.retain", 0)?,
            Some(Value::Bool(true))
        );
        let mut pending = 0usize;
        for i in 0..input_len {
            let topic = kit::str_param(ctx, &self.desc, "topic", i)?;
            let send_input = !matches!(
                ctx.eval_param_at("sendInputData", i)?,
                Some(Value::Bool(false))
            );
            let payload = if send_input {
                Value::Object(ctx.input[0][i].json.clone()).to_string()
            } else {
                kit::str_param(ctx, &self.desc, "message", i)?
            };
            client
                .publish(topic, qos(q), retain, payload.into_bytes())
                .await
                .map_err(|e| kit::err(&name, e.to_string()))?;
            pending += 1;
            out.push(ctx.input[0][i].clone());
        }
        // drive the connection until the broker took everything (QoS 0 has no ack: a short flush)
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut acked = 0usize;
        loop {
            match tokio::time::timeout_at(deadline, events.poll()).await {
                Err(_) => return Err(kit::err(&name, "MQTT broker did not confirm in time")),
                Ok(Err(e)) => return Err(kit::err(&name, format!("MQTT connection failed: {e}"))),
                Ok(Ok(Event::Incoming(Packet::PubAck(_) | Packet::PubComp(_)))) => acked += 1,
                Ok(Ok(Event::Outgoing(rumqttc::Outgoing::Publish(_)))) if q == 0 => acked += 1,
                Ok(Ok(_)) => {}
            }
            if acked >= pending {
                break;
            }
        }
        let _ = client.disconnect().await;
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-mqtt`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(MqttNode {
        desc: description(),
        creds: creds.clone(),
    }))
}
