//! MySQL / MariaDB node (`n8n-nodes-base.mySql`, v2.x): executeQuery, insert, update, select,
//! delete, deleteTable.
//!
//! Statements come from the shared SQL planner (`crate::sql`, SQLite dialect: quoted identifiers,
//! `?N` placeholders). MySQL differs in three ways, handled here: identifiers are double-quoted,
//! so the session runs with `ANSI_QUOTES`; placeholders are positional, so `?N` is rewritten to
//! `?` with the parameters reordered; and there is no `RETURNING`, so writes answer with
//! `{success, affectedRows, insertId}`. Every value is a bound parameter. `upsert` is not offered.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, OptsBuilder, SslOpts};
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{NodeBuilder, Prop};
use crate::kit;
use crate::sql::{self, Dialect, Stmt};

pub const NODE_TYPE: &str = "n8n-nodes-base.mySql";
pub const CREDENTIAL: &str = "mySql";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "MySQL")
        .documentation("https://dev.mysql.com/doc/")
        .property(Prop::string("host", "Host").default("localhost"))
        .property(Prop::number("port", "Port").default(3306))
        .property(Prop::string("database", "Database").default("mysql"))
        .property(Prop::string("user", "User").default("root"))
        .property(Prop::string("password", "Password").password())
        .property(Prop::boolean("ssl", "SSL").description("Connect over TLS"))
        .property(
            Prop::boolean("allowUnauthorizedCerts", "Ignore SSL Issues (Insecure)")
                .description("Accept self-signed or otherwise invalid server certificates"),
        )
}

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "MySQL")
        .description("Get, add and update data in MySQL or MariaDB")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:database")
        .color("#4479A1")
        .credential(CREDENTIAL, true)
        .properties(sql::properties(Dialect::Sqlite))
        .build();
    // no ON CONFLICT upsert in this dialect
    for p in &mut d.properties {
        if p.name == "operation"
            && let Some(opts) = p.options.as_mut()
        {
            opts.retain(|o| o.get("value").and_then(Value::as_str) != Some("upsert"));
        }
    }
    kit::serve_versions(&mut d, &[1.0, 2.0, 2.1, 2.2, 2.3, 2.4, 2.5]);
    d
}

/// `?3 ... ?1` -> `? ... ?` with the parameters in order of appearance. Quoted text is skipped.
pub(crate) fn positional(sql: &str, params: &[Value]) -> Result<(String, Vec<Value>), String> {
    let mut out = String::with_capacity(sql.len());
    let mut ordered = Vec::new();
    let b = sql.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            out.push(char::from(c));
            if c == q {
                quote = None;
            }
            i += 1;
        } else if c == b'\'' || c == b'"' || c == b'`' {
            quote = Some(c);
            out.push(char::from(c));
            i += 1;
        } else if c == b'?' && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let n: usize = sql[i + 1..j].parse().map_err(|_| "bad placeholder")?;
            ordered.push(
                params
                    .get(n.checked_sub(1).ok_or("placeholders start at ?1")?)
                    .cloned()
                    .ok_or_else(|| format!("placeholder ?{n} has no value"))?,
            );
            out.push('?');
            i = j;
        } else {
            // a bare `?` already is positional: take the next unused value
            if c == b'?' {
                ordered.push(params.get(ordered.len()).cloned().unwrap_or(Value::Null));
            }
            // non-ASCII bytes are copied whole below
            let ch = sql[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    Ok((out, ordered))
}

fn to_param(v: &Value) -> mysql_async::Value {
    use mysql_async::Value as M;
    match v {
        Value::Null => M::NULL,
        Value::Bool(b) => M::Int(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .map(M::Int)
            .or_else(|| n.as_u64().map(M::UInt))
            .or_else(|| n.as_f64().map(M::Double))
            .unwrap_or(M::NULL),
        Value::String(s) => M::Bytes(s.clone().into_bytes()),
        other => M::Bytes(other.to_string().into_bytes()),
    }
}

fn from_value(v: mysql_async::Value) -> Value {
    use base64::Engine as _;
    use mysql_async::Value as M;
    match v {
        M::NULL => Value::Null,
        M::Bytes(b) => match String::from_utf8(b) {
            Ok(s) => Value::String(s),
            Err(e) => {
                Value::String(base64::engine::general_purpose::STANDARD.encode(e.into_bytes()))
            }
        },
        M::Int(n) => json!(n),
        M::UInt(n) => json!(n),
        M::Float(f) => {
            serde_json::Number::from_f64(f64::from(f)).map_or(Value::Null, Value::Number)
        }
        M::Double(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
        M::Date(y, mo, d, h, mi, s, us) => {
            if h == 0 && mi == 0 && s == 0 && us == 0 {
                json!(format!("{y:04}-{mo:02}-{d:02}"))
            } else {
                json!(format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}"))
            }
        }
        M::Time(neg, d, h, mi, s, _) => {
            let hours = u32::from(h) + d * 24;
            json!(format!(
                "{}{hours:02}:{mi:02}:{s:02}",
                if neg { "-" } else { "" }
            ))
        }
    }
}

async fn run_stmt(conn: &mut Conn, stmt: &Stmt) -> Result<Vec<Value>, String> {
    let (sql, params) = positional(stmt.sql.trim().trim_end_matches(';'), &stmt.params)?;
    // the shared planner appends RETURNING for engines that have it
    let sql = sql
        .trim_end()
        .strip_suffix("RETURNING *")
        .unwrap_or(&sql)
        .trim_end()
        .to_owned();
    let params = mysql_async::Params::Positional(params.iter().map(to_param).collect());
    let result = conn
        .exec_iter(sql, params)
        .await
        .map_err(|e| e.to_string())?;
    let affected = result.affected_rows();
    let insert_id = result.last_insert_id();
    let names: Vec<String> = result
        .columns_ref()
        .iter()
        .map(|c| c.name_str().into_owned())
        .collect();
    if names.is_empty() {
        result.drop_result().await.map_err(|e| e.to_string())?;
        return Ok(vec![
            json!({"success": true, "affectedRows": affected, "insertId": insert_id}),
        ]);
    }
    let rows: Vec<mysql_async::Row> = result.collect_and_drop().await.map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|mut row| {
            let mut obj = Map::new();
            for (idx, name) in names.iter().enumerate() {
                let v = row
                    .take::<mysql_async::Value, _>(idx)
                    .map_or(Value::Null, from_value);
                obj.insert(name.clone(), v);
            }
            Value::Object(obj)
        })
        .collect())
}

async fn connect(cred: &Map<String, Value>) -> Result<Conn, String> {
    let get = |k: &str| kit::cred_str(cred, k);
    let port = get("port").parse::<u16>().unwrap_or(3306);
    let mut b = OptsBuilder::default()
        .ip_or_hostname(
            Some(get("host"))
                .filter(|h| !h.is_empty())
                .unwrap_or_else(|| "localhost".into()),
        )
        .tcp_port(port)
        .user(Some(get("user")).filter(|u| !u.is_empty()))
        .pass(Some(get("password")).filter(|p| !p.is_empty()))
        .db_name(Some(get("database")).filter(|d| !d.is_empty()));
    if matches!(cred.get("ssl"), Some(Value::Bool(true))) {
        let mut ssl = SslOpts::default();
        if matches!(cred.get("allowUnauthorizedCerts"), Some(Value::Bool(true))) {
            ssl = ssl.with_danger_accept_invalid_certs(true);
        }
        b = b.ssl_opts(Some(ssl));
    }
    let mut conn = tokio::time::timeout(CONNECT_TIMEOUT, Conn::new(b))
        .await
        .map_err(|_| "connection timed out".to_owned())?
        .map_err(|e| e.to_string())?;
    conn.query_drop("SET SESSION sql_mode = CONCAT(@@sql_mode, ',ANSI_QUOTES')")
        .await
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

#[derive(Debug)]
struct MySqlNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

impl MySqlNode {
    fn plan(&self, ctx: &NodeContext, i: usize) -> Result<Stmt, FlowError> {
        match kit::str_param(ctx, &self.desc, "operation", i)?.as_str() {
            "executeQuery" => sql::plan_query(ctx, Dialect::Sqlite, i),
            op => sql::plan_structured(ctx, op, Dialect::Sqlite, i),
        }
    }
}

#[async_trait]
impl FlowNode for MySqlNode {
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
        let mut conn = connect(&cred)
            .await
            .map_err(|m| kit::err(name, format!("Could not connect: {m}")))?;
        let mut out = Vec::new();
        for i in 0..input_len {
            let res = match self.plan(ctx, i) {
                Ok(stmt) => run_stmt(&mut conn, &stmt)
                    .await
                    .map(|rows| rows.into_iter().map(kit::to_item).collect::<Vec<Item>>())
                    .map_err(|m| kit::err(name, m)),
                Err(e) => Err(e),
            };
            kit::settle(ctx, i, res, &mut out)?;
        }
        let _ = conn.disconnect().await;
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-mysql`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(MySqlNode {
        desc: description(),
        creds: creds.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_placeholders_become_positional_in_order() {
        let (sql, p) = positional(
            "UPDATE \"t\" SET \"a\" = ?2, \"b\" = ?1 WHERE \"id\" = ?3 AND note = '?1 stays'",
            &[json!("x"), json!("y"), json!(7)],
        )
        .unwrap();
        assert_eq!(
            sql,
            "UPDATE \"t\" SET \"a\" = ?, \"b\" = ? WHERE \"id\" = ? AND note = '?1 stays'"
        );
        assert_eq!(p, vec![json!("y"), json!("x"), json!(7)]);
        assert!(positional("SELECT ?5", &[json!(1)]).is_err());
    }
}
