//! SQLite node (`hokkabaz.sqlite`): executeQuery, insert, update, upsert, select, delete,
//! deleteTable against a local database file.
//!
//! rusqlite with the bundled library, same version/features as hk-session, so the workspace
//! links a single sqlite3 copy. All values are bound parameters (see [`crate::sql`]).
//! The file path is a credential (`sqliteDb`) so a flow never carries host paths in its JSON.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use rusqlite::types::{Value as Sql, ValueRef};
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{NodeBuilder, Prop};
use crate::kit::{self, node_err};
use crate::sql::{self, Dialect, Stmt};

pub const NODE_TYPE: &str = "hokkabaz.sqlite";
pub const CREDENTIAL: &str = "sqliteDb";

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "SQLite Database").property(
        Prop::string("path", "Database File")
            .required()
            .placeholder("/path/to/data.db")
            .description(
                "Path of the SQLite file on the machine running the flow. Created if missing",
            ),
    )
}

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "SQLite")
        .description("Get, add, update and delete rows in a SQLite database")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:database")
        .color("#003b57")
        .credential(CREDENTIAL, true)
        .properties(sql::properties(Dialect::Sqlite))
        .build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

fn to_sql(v: &Value) -> Sql {
    match v {
        Value::Null => Sql::Null,
        Value::Bool(b) => Sql::Integer(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .map(Sql::Integer)
            .unwrap_or_else(|| Sql::Real(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => Sql::Text(s.clone()),
        other => Sql::Text(other.to_string()),
    }
}

fn from_sql(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => json!(n),
        ValueRef::Real(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::String(base64::engine::general_purpose::STANDARD.encode(b)),
    }
}

/// Run one statement. Row-returning statements give one JSON object per row, others a
/// `{success, changes}` summary.
pub(crate) fn run_stmt(conn: &rusqlite::Connection, stmt: &Stmt) -> Result<Vec<Value>, String> {
    let params = rusqlite::params_from_iter(stmt.params.iter().map(to_sql));
    let mut prep = match conn.prepare(&stmt.sql) {
        Ok(p) => p,
        // several statements: only runnable without parameters
        Err(rusqlite::Error::MultipleStatement) if stmt.params.is_empty() => {
            conn.execute_batch(&stmt.sql).map_err(|e| e.to_string())?;
            return Ok(vec![json!({ "success": true })]);
        }
        Err(e) => return Err(e.to_string()),
    };
    if prep.column_count() == 0 {
        let n = prep.execute(params).map_err(|e| e.to_string())?;
        return Ok(vec![json!({ "success": true, "changes": n })]);
    }
    let names: Vec<String> = prep.column_names().into_iter().map(str::to_owned).collect();
    let mut rows = prep.query(params).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let mut obj = Map::new();
        for (idx, name) in names.iter().enumerate() {
            let v = row.get_ref(idx).map(from_sql).map_err(|e| e.to_string())?;
            obj.insert(name.clone(), v);
        }
        out.push(Value::Object(obj));
    }
    Ok(out)
}

fn open(path: &str) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open(path)
        .map_err(|e| format!("Could not open the database: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

#[derive(Debug)]
struct SqliteNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

impl SqliteNode {
    fn plan(&self, ctx: &NodeContext, i: usize) -> Result<Stmt, FlowError> {
        match kit::str_param(ctx, &self.desc, "operation", i)?.as_str() {
            "executeQuery" => sql::plan_query(ctx, Dialect::Sqlite, i),
            op => sql::plan_structured(ctx, op, Dialect::Sqlite, i),
        }
    }
}

#[async_trait]
impl FlowNode for SqliteNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let input_len = ctx.input.first().map_or(0, Vec::len);
        if input_len == 0 {
            return Ok(vec![Vec::new()]);
        }
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let path = kit::cred_str(&cred, "path");
        if path.trim().is_empty() {
            return Err(node_err(ctx, "The credential has no database file"));
        }
        // evaluate expressions here (needs the node context), execute in one blocking task
        let plans: Vec<Result<Stmt, FlowError>> =
            (0..input_len).map(|i| self.plan(ctx, i)).collect();
        let (stmts, errs): (Vec<_>, Vec<_>) = plans
            .into_iter()
            .map(|p| match p {
                Ok(s) => (Some(s), None),
                Err(e) => (None, Some(e)),
            })
            .unzip();
        let name = ctx.node.name.clone();
        let results = tokio::task::spawn_blocking(move || {
            let conn = open(&path)?;
            Ok::<_, String>(
                stmts
                    .into_iter()
                    .map(|s| s.map(|s| run_stmt(&conn, &s)))
                    .collect::<Vec<_>>(),
            )
        })
        .await
        .map_err(|e| kit::err(&name, format!("The database task failed: {e}")))?
        .map_err(|m| kit::err(&name, m))?;

        let mut out = Vec::new();
        for (i, (res, plan_err)) in results.into_iter().zip(errs).enumerate() {
            let res: Result<Vec<Item>, FlowError> = match (res, plan_err) {
                (_, Some(e)) => Err(e),
                (Some(Ok(rows)), None) => Ok(rows.into_iter().map(kit::to_item).collect()),
                (Some(Err(m)), None) => Err(kit::err(&name, m)),
                (None, None) => Ok(Vec::new()),
            };
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-sqlite`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(SqliteNode {
        desc: description(),
        creds: creds.clone(),
    }))
}
