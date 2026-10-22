//! Kafka (`n8n-nodes-base.kafka`): produce messages to a topic, or read from a partition.
//! Pure-Rust protocol client (`rskafka`); SASL/PLAIN and TLS supported. Reads are by partition and
//! offset (no consumer group), which suits "give me what is new since offset N" workflows.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rskafka::client::partition::{Compression, OffsetAt, UnknownTopicHandling};
use rskafka::client::{ClientBuilder, Credentials, SaslConfig};
use rskafka::record::Record;
use serde_json::{Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop};
use crate::kit;

pub const NODE_TYPE: &str = "n8n-nodes-base.kafka";
pub const CREDENTIAL: &str = "kafka";

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "Kafka")
        .property(
            Prop::string("brokers", "Brokers")
                .default("localhost:9092")
                .required()
                .description("Comma separated host:port"),
        )
        .property(Prop::string("clientId", "Client ID").default("hokkabaz"))
        .property(Prop::boolean("ssl", "SSL"))
        .property(Prop::boolean(
            "allowUnauthorizedCerts",
            "Ignore SSL Issues (Insecure)",
        ))
        .property(
            Prop::string("username", "SASL Username")
                .description("SASL/PLAIN; empty: no authentication"),
        )
        .property(Prop::string("password", "SASL Password").password())
        .property(
            Prop::options(
                "mechanism",
                "SASL Mechanism",
                vec![
                    Choice::new("PLAIN", "plain"),
                    Choice::new("SCRAM-SHA-256", "scram-sha-256"),
                    Choice::new("SCRAM-SHA-512", "scram-sha-512"),
                ],
            )
            .default("plain"),
        )
}

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "Kafka")
        .description("Produce to and read from Kafka topics")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:project-diagram")
        .color("#231F20")
        .credential(CREDENTIAL, true)
        .property(Prop::options(
            "operation",
            "Operation",
            vec![
                Choice::new("Send Message", "produce").action("Send messages to a topic"),
                Choice::new("Read Messages", "consume").action("Read messages from a partition"),
            ],
        ))
        .property(Prop::string("topic", "Topic").required())
        .property(Prop::number("partition", "Partition").default(0))
        .property(
            Prop::boolean("sendInputData", "Send Input Data")
                .default(true)
                .show("operation", &["produce"]),
        )
        .property(
            Prop::string("message", "Message")
                .show("operation", &["produce"])
                .show_bool("sendInputData", false),
        )
        .property(Prop::string("key", "Key").show("operation", &["produce"]))
        .property(
            Prop::options(
                "from",
                "Start At",
                vec![
                    Choice::new("Earliest", "earliest"),
                    Choice::new("Latest (only new)", "latest"),
                    Choice::new("Offset", "offset"),
                ],
            )
            .default("earliest")
            .show("operation", &["consume"]),
        )
        .property(
            Prop::number("offset", "Offset")
                .show("operation", &["consume"])
                .show("from", &["offset"]),
        )
        .property(
            Prop::number("maxMessages", "Max Messages")
                .default(10)
                .show("operation", &["consume"]),
        )
        .property(
            Prop::number("timeoutSeconds", "Wait (seconds)")
                .default(5)
                .show("operation", &["consume"]),
        )
        .build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

#[derive(Debug)]
struct KafkaNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

#[async_trait]
impl FlowNode for KafkaNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let name = ctx.node.name.clone();
        let e = |m: String| kit::err(&name, m);
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let brokers: Vec<String> = kit::cred_str(&cred, "brokers")
            .split(',')
            .map(|b| b.trim().to_owned())
            .filter(|b| !b.is_empty())
            .collect();
        if brokers.is_empty() {
            return Err(e("The credential lists no brokers".into()));
        }
        let mut b = ClientBuilder::new(brokers).client_id(
            Some(kit::cred_str(&cred, "clientId"))
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| "hokkabaz".into()),
        );
        if matches!(cred.get("ssl"), Some(Value::Bool(true))) {
            let insecure = matches!(cred.get("allowUnauthorizedCerts"), Some(Value::Bool(true)));
            b = b.tls_config(crate::tls::client_config(insecure).map_err(&e)?);
        }
        let user = kit::cred_str(&cred, "username");
        if !user.is_empty() {
            let c = Credentials {
                username: user,
                password: kit::cred_str(&cred, "password"),
            };
            b = b.sasl_config(match kit::cred_str(&cred, "mechanism").as_str() {
                "scram-sha-256" => SaslConfig::ScramSha256(c),
                "scram-sha-512" => SaslConfig::ScramSha512(c),
                _ => SaslConfig::Plain(c),
            });
        }
        let client = tokio::time::timeout(Duration::from_secs(20), b.build())
            .await
            .map_err(|_| e("connection timed out".into()))?
            .map_err(|x| e(format!("Could not connect: {x}")))?;
        let topic = kit::str_param(ctx, &self.desc, "topic", 0)?;
        let part = ctx
            .eval_param_at("partition", 0)?
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32;
        let pc = client
            .partition_client(topic.clone(), part, UnknownTopicHandling::Retry)
            .await
            .map_err(|x| e(format!("topic `{topic}` partition {part}: {x}")))?;
        let op = kit::str_param(ctx, &self.desc, "operation", 0)?;
        let mut out = Vec::new();
        if op == "consume" {
            let from = kit::str_param(ctx, &self.desc, "from", 0)?;
            let mut offset = match from.as_str() {
                "latest" => pc
                    .get_offset(OffsetAt::Latest)
                    .await
                    .map_err(|x| e(x.to_string()))?,
                "offset" => ctx
                    .eval_param_at("offset", 0)?
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
                _ => pc
                    .get_offset(OffsetAt::Earliest)
                    .await
                    .map_err(|x| e(x.to_string()))?,
            };
            let max = ctx
                .eval_param_at("maxMessages", 0)?
                .and_then(|v| v.as_u64())
                .unwrap_or(10)
                .max(1) as usize;
            let wait = ctx
                .eval_param_at("timeoutSeconds", 0)?
                .and_then(|v| v.as_u64())
                .unwrap_or(5)
                .clamp(1, 120);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
            while out.len() < max && tokio::time::Instant::now() < deadline {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                let fetched = pc
                    .fetch_records(
                        offset,
                        1..4_000_000,
                        i32::try_from(left.as_millis().min(5000)).unwrap_or(1000),
                    )
                    .await
                    .map_err(|x| e(x.to_string()))?;
                if fetched.0.is_empty() {
                    continue;
                }
                for r in fetched.0 {
                    offset = r.offset + 1;
                    let text = r
                        .record
                        .value
                        .as_deref()
                        .map(|v| String::from_utf8_lossy(v).into_owned())
                        .unwrap_or_default();
                    let message =
                        serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
                    out.push(Item::from_json(json!({
                        "topic": topic, "partition": part, "offset": r.offset, "message": message,
                        "key": r.record.key.as_deref().map(|k| String::from_utf8_lossy(k).into_owned()),
                        "timestamp": r.record.timestamp.to_rfc3339(),
                    })));
                    if out.len() >= max {
                        break;
                    }
                }
            }
            return Ok(vec![out]);
        }
        let key = Some(kit::str_param(ctx, &self.desc, "key", 0)?).filter(|k| !k.is_empty());
        let mut records = Vec::new();
        for (i, item) in ctx.input.first().into_iter().flatten().enumerate() {
            let payload = if matches!(
                ctx.eval_param_at("sendInputData", i)?,
                Some(Value::Bool(false))
            ) {
                kit::str_param(ctx, &self.desc, "message", i)?
            } else {
                Value::Object(item.json.clone()).to_string()
            };
            records.push(Record {
                key: key.clone().map(String::into_bytes),
                value: Some(payload.into_bytes()),
                headers: Default::default(),
                timestamp: chrono_now(),
            });
            out.push(item.clone());
        }
        pc.produce(records, Compression::NoCompression)
            .await
            .map_err(|x| e(format!("produce failed: {x}")))?;
        Ok(vec![out])
    }
}

fn chrono_now() -> rskafka::chrono::DateTime<rskafka::chrono::Utc> {
    rskafka::chrono::Utc::now()
}

/// Node and credential type into the shared registries. Features: `int-kafka`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(KafkaNode {
        desc: description(),
        creds: creds.clone(),
    }))
}
