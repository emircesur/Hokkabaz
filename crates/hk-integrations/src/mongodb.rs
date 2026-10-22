//! MongoDB (`n8n-nodes-base.mongoDb`): find, insert, update, delete and aggregate on a
//! collection. JSON in, JSON out: `_id` and dates come back as plain strings. Desktop and server
//! builds (the driver is not built for Android).

use std::sync::Arc;

use async_trait::async_trait;
use mongodb::Client;
use mongodb::bson::{self, Bson, Document};
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop};
use crate::kit;

pub const NODE_TYPE: &str = "n8n-nodes-base.mongoDb";
pub const CREDENTIAL: &str = "mongoDb";

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "MongoDB")
        .property(
            Prop::string("connectionString", "Connection String")
                .password()
                .required()
                .default("mongodb://localhost:27017")
                .description("mongodb:// or mongodb+srv:// URI, with user and password"),
        )
        .property(Prop::string("database", "Database").required())
}

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "MongoDB")
        .description("Find, insert and update documents in MongoDB")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:database")
        .color("#13AA52")
        .credential(CREDENTIAL, true)
        .property(Prop::options(
            "operation",
            "Operation",
            vec![
                Choice::new("Find", "find").action("Find documents"),
                Choice::new("Insert", "insert").action("Insert documents"),
                Choice::new("Update", "update").action("Update documents"),
                Choice::new("Delete", "delete").action("Delete documents"),
                Choice::new("Aggregate", "aggregate").action("Run an aggregation pipeline"),
            ],
        ))
        .property(Prop::string("collection", "Collection").required())
        .property(
            Prop::json("query", "Query (JSON)")
                .default("{}")
                .show("operation", &["find", "update", "delete"]),
        )
        .property(
            Prop::json("sort", "Sort (JSON)")
                .default("{}")
                .show("operation", &["find"]),
        )
        .property(
            Prop::number("limit", "Limit")
                .default(50)
                .show("operation", &["find"]),
        )
        .property(
            Prop::json("update", "Update (JSON)")
                .default("{\"$set\": {}}")
                .show("operation", &["update"]),
        )
        .property(Prop::boolean("upsert", "Upsert").show("operation", &["update"]))
        .property(
            Prop::json("pipeline", "Pipeline (JSON array)")
                .default("[]")
                .show("operation", &["aggregate"]),
        )
        .property(
            Prop::boolean("sendInputData", "Insert Input Data")
                .default(true)
                .show("operation", &["insert"]),
        )
        .property(
            Prop::json("document", "Document (JSON)")
                .default("{}")
                .show("operation", &["insert"])
                .show_bool("sendInputData", false),
        )
        .build();
    kit::serve_versions(&mut d, &[1.0, 1.1]);
    d
}

fn json_param(ctx: &NodeContext, name: &str, i: usize, default: &str) -> Result<Value, FlowError> {
    let v = ctx.eval_param_at(name, i)?;
    match v {
        Some(Value::String(s)) if !s.trim().is_empty() => serde_json::from_str(&s)
            .map_err(|e| kit::node_err(ctx, format!("Parameter `{name}` is not valid JSON: {e}"))),
        Some(Value::String(_)) | None => Ok(serde_json::from_str(default).unwrap_or(Value::Null)),
        Some(other) => Ok(other),
    }
}

fn doc_of(ctx: &NodeContext, v: &Value, what: &str) -> Result<Document, FlowError> {
    match bson::to_bson(v) {
        Ok(Bson::Document(d)) => Ok(d),
        _ => Err(kit::node_err(ctx, format!("{what} must be a JSON object"))),
    }
}

/// Extended JSON -> plain JSON: `{"$oid": x}` is `x`, `{"$date": ..}` an ISO string, numbers unwrap.
fn plain(v: Value) -> Value {
    match v {
        Value::Object(m) if m.len() == 1 => {
            let (k, x) = m.into_iter().next().expect("one entry");
            match (k.as_str(), x) {
                ("$oid", Value::String(s)) => Value::String(s),
                ("$date", Value::String(s)) => Value::String(s),
                ("$date", Value::Object(d)) => d.get("$numberLong").cloned().unwrap_or(Value::Null),
                (
                    "$numberLong" | "$numberInt" | "$numberDouble" | "$numberDecimal",
                    Value::String(s),
                ) => s
                    .parse::<i64>()
                    .map(Value::from)
                    .or_else(|_| s.parse::<f64>().map(|f| json!(f)))
                    .unwrap_or(Value::String(s)),
                (k, x) => {
                    let mut o = Map::new();
                    o.insert(k.to_owned(), plain(x));
                    Value::Object(o)
                }
            }
        }
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, x)| (k, plain(x))).collect()),
        Value::Array(a) => Value::Array(a.into_iter().map(plain).collect()),
        other => other,
    }
}

fn out_json(d: Document) -> Value {
    plain(Bson::Document(d).into_relaxed_extjson())
}

#[derive(Debug)]
struct MongoNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

#[async_trait]
impl FlowNode for MongoNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let input_len = ctx.input.first().map_or(0, Vec::len);
        if input_len == 0 {
            return Ok(vec![Vec::new()]);
        }
        let name = ctx.node.name.clone();
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let client = Client::with_uri_str(kit::cred_str(&cred, "connectionString"))
            .await
            // the URI carries the password: never echo it
            .map_err(|_| kit::err(&name, "Invalid MongoDB connection string"))?;
        let coll_name = kit::str_param(ctx, &self.desc, "collection", 0)?;
        let db = client.database(&kit::cred_str(&cred, "database"));
        let coll = db.collection::<Document>(&coll_name);
        let op = kit::str_param(ctx, &self.desc, "operation", 0)?;
        let mut out = Vec::new();
        for i in 0..input_len {
            let res: Result<Vec<Item>, FlowError> = async {
                let e = |m: String| kit::err(&name, m);
                Ok(match op.as_str() {
                    "find" => {
                        let filter = doc_of(ctx, &json_param(ctx, "query", i, "{}")?, "Query")?;
                        let sort = doc_of(ctx, &json_param(ctx, "sort", i, "{}")?, "Sort")?;
                        let limit = ctx.eval_param_at("limit", i)?.and_then(|v| v.as_i64()).unwrap_or(50);
                        let mut cur = coll.find(filter).sort(sort).limit(limit).await.map_err(|x| e(x.to_string()))?;
                        let mut items = Vec::new();
                        while cur.advance().await.map_err(|x| e(x.to_string()))? {
                            items.push(kit::to_item(out_json(cur.deserialize_current().map_err(|x| e(x.to_string()))?)));
                        }
                        items
                    }
                    "insert" => {
                        let v = if matches!(ctx.eval_param_at("sendInputData", i)?, Some(Value::Bool(false))) {
                            json_param(ctx, "document", i, "{}")?
                        } else {
                            Value::Object(ctx.input[0][i].json.clone())
                        };
                        let r = coll.insert_one(doc_of(ctx, &v, "The document")?).await.map_err(|x| e(x.to_string()))?;
                        vec![kit::to_item(json!({"acknowledged": true, "insertedId": plain(r.inserted_id.into_relaxed_extjson())}))]
                    }
                    "update" => {
                        let filter = doc_of(ctx, &json_param(ctx, "query", i, "{}")?, "Query")?;
                        let upd = doc_of(ctx, &json_param(ctx, "update", i, "{}")?, "Update")?;
                        let upsert = matches!(ctx.eval_param_at("upsert", i)?, Some(Value::Bool(true)));
                        let r = coll.update_many(filter, upd).upsert(upsert).await.map_err(|x| e(x.to_string()))?;
                        vec![kit::to_item(json!({"matchedCount": r.matched_count, "modifiedCount": r.modified_count, "upsertedId": r.upserted_id.map(|b| plain(b.into_relaxed_extjson()))}))]
                    }
                    "delete" => {
                        let filter = doc_of(ctx, &json_param(ctx, "query", i, "{}")?, "Query")?;
                        let r = coll.delete_many(filter).await.map_err(|x| e(x.to_string()))?;
                        vec![kit::to_item(json!({"deletedCount": r.deleted_count}))]
                    }
                    "aggregate" => {
                        let Value::Array(stages) = json_param(ctx, "pipeline", i, "[]")? else {
                            return Err(kit::node_err(ctx, "The pipeline must be a JSON array"));
                        };
                        let pipeline = stages.iter().map(|s| doc_of(ctx, s, "A pipeline stage")).collect::<Result<Vec<_>, _>>()?;
                        let mut cur = coll.aggregate(pipeline).await.map_err(|x| e(x.to_string()))?;
                        let mut items = Vec::new();
                        while cur.advance().await.map_err(|x| e(x.to_string()))? {
                            items.push(kit::to_item(out_json(cur.deserialize_current().map_err(|x| e(x.to_string()))?)));
                        }
                        items
                    }
                    other => return Err(e(format!("Unknown operation `{other}`"))),
                })
            }
            .await;
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-mongodb`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(MongoNode {
        desc: description(),
        creds: creds.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_json_becomes_plain() {
        let v = json!({"_id": {"$oid": "65a"}, "at": {"$date": "2024-01-02T00:00:00Z"}, "n": {"$numberLong": "5"}, "tags": [{"$oid": "1"}]});
        assert_eq!(
            plain(v),
            json!({"_id": "65a", "at": "2024-01-02T00:00:00Z", "n": 5, "tags": ["1"]})
        );
    }
}
