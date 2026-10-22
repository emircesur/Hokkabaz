//! PostgreSQL node (`n8n-nodes-base.postgres`, v2.x): executeQuery, insert, update, upsert,
//! select, delete, deleteTable.
//!
//! tokio-postgres over tokio-rustls (ring, webpki roots): no openssl, android-safe.
//! Values are bound parameters; `executeQuery` binds `{{ }}` expressions and
//! `options.queryReplacement` as `$1..$n` (see [`crate::sql`]).
//!
//! Limits: one statement per query (extended protocol); bound parameters of the user query
//! are bool, int, float, text and json(b) typed - for anything else cast through text
//! (`{{ $json.price }}::text::numeric`). Result rows go through `to_jsonb`, so every column
//! type comes back as JSON.

use std::error::Error as StdError;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::BytesMut;
use serde_json::{Value, json};
use tokio_postgres::types::{IsNull, ToSql, Type, to_sql_checked};
use tokio_postgres::{Client, Config, NoTls, config::SslMode};
use tokio_postgres_rustls::MakeRustlsConnect;

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop};
use crate::kit;
use crate::sql::{self, Dialect, Stmt};
use crate::template::display;

pub const NODE_TYPE: &str = "n8n-nodes-base.postgres";
pub const CREDENTIAL: &str = "postgres";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "Postgres")
        .documentation("https://www.postgresql.org/docs/current/libpq-connect.html")
        .property(Prop::string("host", "Host").default("localhost"))
        .property(Prop::string("database", "Database").default("postgres"))
        .property(Prop::string("user", "User").default("postgres"))
        .property(Prop::string("password", "Password").password())
        .property(Prop::number("maxConnections", "Max Connections").default(100).description("Unused: the node opens one connection per execution"))
        .property(Prop::boolean("allowUnauthorizedCerts", "Ignore SSL Issues (Insecure)").description("Accept self-signed or otherwise invalid server certificates"))
        .property(Prop::options(
            "ssl",
            "SSL",
            vec![
                Choice::new("Disable", "disable"),
                Choice::new("Allow", "allow"),
                Choice::new("Require", "require"),
                Choice::new("Verify (Not Implemented)", "verify"),
                Choice::new("Verify-Full (Not Implemented)", "verify-full"),
            ],
        ))
        .property(Prop::number("port", "Port").default(5432))
}

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "Postgres")
        .description("Get, add and update data in Postgres")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:database")
        .color("#336791")
        .credential(CREDENTIAL, true)
        .properties(sql::properties(Dialect::Postgres))
        .build();
    kit::serve_versions(&mut d, &[2.0, 2.1, 2.2, 2.3, 2.4, 2.5, 2.6]);
    d
}

type BoxErr = Box<dyn StdError + Sync + Send>;

/// A JSON value sent as whatever type the server inferred for the parameter.
#[derive(Debug)]
struct Param<'a>(&'a Value);

fn int_of(v: &Value) -> Result<i64, BoxErr> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
    .ok_or_else(|| format!("`{}` is not an integer", display(v)).into())
}

fn float_of(v: &Value) -> Result<f64, BoxErr> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .ok_or_else(|| format!("`{}` is not a number", display(v)).into())
}

fn bool_of(v: &Value) -> Result<bool, BoxErr> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_i64().map(|n| n != 0),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" => Some(true),
            "false" | "f" | "0" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
    .ok_or_else(|| format!("`{}` is not a boolean", display(v)).into())
}

impl ToSql for Param<'_> {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, BoxErr> {
        let v = self.0;
        if v.is_null() {
            return Ok(IsNull::Yes);
        }
        match *ty {
            Type::BOOL => bool_of(v)?.to_sql(ty, out),
            Type::INT2 => i16::try_from(int_of(v)?)?.to_sql(ty, out),
            Type::INT4 => i32::try_from(int_of(v)?)?.to_sql(ty, out),
            Type::INT8 => int_of(v)?.to_sql(ty, out),
            Type::FLOAT4 => (float_of(v)? as f32).to_sql(ty, out),
            Type::FLOAT8 => float_of(v)?.to_sql(ty, out),
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => display(v).to_sql(ty, out),
            Type::JSON | Type::JSONB => v.to_sql(ty, out),
            _ => Err(format!("parameter type `{ty}` is not supported; cast it through text, e.g. `$1::text::{ty}`").into()),
        }
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

fn db_err(e: &tokio_postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => format!("{} (code {})", db.message(), db.code().code()),
        None => e.to_string(),
    }
}

/// Run one statement; rows come back as JSON objects.
async fn run_stmt(client: &Client, stmt: &Stmt) -> Result<Vec<Value>, String> {
    let sql = stmt.sql.trim().trim_end_matches(';').trim_end();
    let params: Vec<Param<'_>> = stmt.params.iter().map(Param).collect();
    let refs: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();

    // Rows through to_jsonb so every column type is decodable. DML without RETURNING (and
    // EXPLAIN, SHOW, ...) cannot sit in a CTE: those fall through to the plain path.
    let wrapped = format!("WITH q AS ({sql}) SELECT to_jsonb(q) AS row FROM q");
    if let Ok(prepared) = client.prepare(&wrapped).await {
        let rows = client.query(&prepared, &refs).await.map_err(|e| db_err(&e))?;
        return rows.iter().map(|r| r.try_get::<_, Value>(0).map_err(|e| db_err(&e))).collect();
    }

    let prepared = client.prepare(sql).await.map_err(|e| db_err(&e))?;
    if prepared.columns().is_empty() {
        let n = client.execute(&prepared, &refs).await.map_err(|e| db_err(&e))?;
        return Ok(vec![json!({ "success": true, "rowCount": n })]);
    }
    let rows = client.query(&prepared, &refs).await.map_err(|e| db_err(&e))?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let mut obj = serde_json::Map::new();
        for (idx, col) in r.columns().iter().enumerate() {
            let v = match *col.type_() {
                Type::BOOL => r.try_get::<_, Option<bool>>(idx).map(|v| json!(v)),
                Type::INT2 => r.try_get::<_, Option<i16>>(idx).map(|v| json!(v)),
                Type::INT4 => r.try_get::<_, Option<i32>>(idx).map(|v| json!(v)),
                Type::INT8 => r.try_get::<_, Option<i64>>(idx).map(|v| json!(v)),
                Type::FLOAT4 => r.try_get::<_, Option<f32>>(idx).map(|v| json!(v)),
                Type::FLOAT8 => r.try_get::<_, Option<f64>>(idx).map(|v| json!(v)),
                Type::JSON | Type::JSONB => r.try_get::<_, Option<Value>>(idx).map(|v| json!(v)),
                Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => {
                    r.try_get::<_, Option<String>>(idx).map(|v| json!(v))
                }
                ref other => return Err(format!("column `{}` has unsupported type `{other}`", col.name())),
            }
            .map_err(|e| db_err(&e))?;
            obj.insert(col.name().to_owned(), v);
        }
        out.push(Value::Object(obj));
    }
    Ok(out)
}

struct Settings {
    config: Config,
    tls: bool,
    /// `ssl = allow`: plain first, TLS when that fails.
    fallback_tls: bool,
    insecure: bool,
}

fn settings(cred: &serde_json::Map<String, Value>) -> Settings {
    let get = |k: &str| kit::cred_str(cred, k);
    let mode = get("ssl");
    let mut config = Config::new();
    let host = get("host");
    config.host(if host.is_empty() { "localhost" } else { &host });
    config.port(get("port").parse().unwrap_or(5432));
    config.dbname(get("database"));
    config.user(get("user"));
    config.password(get("password"));
    config.connect_timeout(CONNECT_TIMEOUT);
    config.application_name("hokkabaz");
    let tls = !matches!(mode.as_str(), "" | "disable" | "allow");
    config.ssl_mode(if tls { SslMode::Require } else { SslMode::Disable });
    Settings {
        config,
        tls,
        fallback_tls: mode == "allow",
        insecure: matches!(cred.get("allowUnauthorizedCerts"), Some(Value::Bool(true))),
    }
}

async fn connect_plain(config: &Config) -> Result<Client, String> {
    let (client, conn) = config.connect(NoTls).await.map_err(|e| db_err(&e))?;
    tokio::spawn(conn);
    Ok(client)
}

async fn connect_tls(config: &Config, insecure: bool) -> Result<Client, String> {
    let cfg = crate::tls::client_config(insecure)?;
    let (client, conn) = config.connect(MakeRustlsConnect::new((*cfg).clone())).await.map_err(|e| db_err(&e))?;
    tokio::spawn(conn);
    Ok(client)
}

async fn connect(s: &Settings) -> Result<Client, String> {
    if s.tls {
        return connect_tls(&s.config, s.insecure).await;
    }
    match connect_plain(&s.config).await {
        Err(e) if s.fallback_tls => {
            let mut c = s.config.clone();
            c.ssl_mode(SslMode::Require);
            connect_tls(&c, s.insecure).await.map_err(|e2| format!("{e}; TLS: {e2}"))
        }
        other => other,
    }
}

#[derive(Debug)]
struct PostgresNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

impl PostgresNode {
    fn plan(&self, ctx: &NodeContext, i: usize) -> Result<Stmt, FlowError> {
        match kit::str_param(ctx, &self.desc, "operation", i)?.as_str() {
            "executeQuery" => sql::plan_query(ctx, Dialect::Postgres, i),
            op => sql::plan_structured(ctx, op, Dialect::Postgres, i),
        }
    }
}

#[async_trait]
impl FlowNode for PostgresNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let input_len = ctx.input.first().map_or(0, Vec::len);
        if input_len == 0 {
            return Ok(vec![Vec::new()]);
        }
        let name = &ctx.node.name;
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let client = connect(&settings(&cred)).await.map_err(|m| kit::err(name, format!("Could not connect: {m}")))?;
        let mut out = Vec::new();
        for i in 0..input_len {
            let res = match self.plan(ctx, i) {
                Ok(stmt) => run_stmt(&client, &stmt)
                    .await
                    .map(|rows| rows.into_iter().map(kit::to_item).collect::<Vec<Item>>())
                    .map_err(|m| kit::err(name, m)),
                Err(e) => Err(e),
            };
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-postgres`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(PostgresNode { desc: description(), creds: creds.clone() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_maps_to_connection_settings() {
        let cred = json!({"host": "db.local", "port": "6543", "database": "app", "user": "u", "password": "p", "ssl": "require", "allowUnauthorizedCerts": true});
        let s = settings(cred.as_object().unwrap());
        assert!(s.tls && s.insecure && !s.fallback_tls);
        assert_eq!(s.config.get_ports(), &[6543]);
        let s = settings(json!({"ssl": "allow"}).as_object().unwrap());
        assert!(!s.tls && s.fallback_tls);
        let s = settings(&serde_json::Map::new());
        assert!(!s.tls && s.config.get_ports() == [5432]);
    }

    #[test]
    fn params_convert_by_server_type() {
        let mut buf = BytesMut::new();
        assert!(matches!(Param(&json!("42")).to_sql(&Type::INT4, &mut buf), Ok(IsNull::No)));
        assert!(Param(&json!("x")).to_sql(&Type::INT4, &mut buf).is_err());
        assert!(Param(&json!(70000)).to_sql(&Type::INT2, &mut buf).is_err());
        assert!(matches!(Param(&Value::Null).to_sql(&Type::UUID, &mut buf), Ok(IsNull::Yes)));
        let Err(e) = Param(&json!("1.5")).to_sql(&Type::NUMERIC, &mut buf) else { panic!("numeric must be rejected") };
        let e = e.to_string();
        assert!(e.contains("::text::numeric"), "{e}");
    }
}
