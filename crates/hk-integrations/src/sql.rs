//! SQL plumbing shared by the SQLite and PostgreSQL nodes.
//!
//! Two rules keep node input out of the SQL text:
//! - `{{ expr }}` inside a query is never interpolated. Each one is evaluated on its own and
//!   becomes a bound parameter (`?N` / `$N`); see [`bind_query`].
//! - The structured operations (insert/update/select/...) build SQL from quoted identifiers
//!   and placeholders only; every value travels as a parameter.
//!
//! Postgres is strongly typed on the wire and the builders do not know column types, so
//! values reach it as JSON through `jsonb_populate_record(NULL::table, $n)`, which lets the
//! server do the text -> column type conversion (numeric, uuid, timestamptz, ...).

// Which variants and helpers are used depends on which of `int-sqlite` / `int-postgres` is on.
#![allow(dead_code)]

use serde_json::{Map, Value};

use hk_flow::{FlowError, NodeContext};

use crate::describe::{Choice, Prop, return_all_and_limit};
use crate::template::display;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    Sqlite,
    Postgres,
}

impl Dialect {
    pub fn ph(self, n: usize) -> String {
        match self {
            Dialect::Sqlite => format!("?{n}"),
            Dialect::Postgres => format!("${n}"),
        }
    }
}

/// SQL text plus its bound parameters (JSON; each driver converts).
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Stmt {
    pub sql: String,
    pub params: Vec<Value>,
}

pub(crate) fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Table {
    pub schema: Option<String>,
    pub name: String,
}

impl Table {
    pub fn sql(&self) -> String {
        match &self.schema {
            Some(s) if !s.is_empty() => format!("{}.{}", quote_ident(s), quote_ident(&self.name)),
            _ => quote_ident(&self.name),
        }
    }
}

/// n8n resource locators (`{__rl: true, value: "users", mode: "list"}`) or plain strings.
pub(crate) fn rl_value(v: &Value) -> String {
    match v {
        Value::Object(m) => m.get("value").map(display).unwrap_or_default(),
        other => display(other),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Like,
    IsNull,
    IsNotNull,
}

impl Op {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "equal" | "=" => Op::Eq,
            "notEqual" | "!=" | "<>" => Op::Ne,
            "larger" | ">" => Op::Gt,
            "largerEqual" | ">=" => Op::Ge,
            "smaller" | "<" => Op::Lt,
            "smallerEqual" | "<=" => Op::Le,
            "like" | "LIKE" => Op::Like,
            "isNull" | "IS NULL" => Op::IsNull,
            "isNotNull" | "IS NOT NULL" => Op::IsNotNull,
            _ => return None,
        })
    }

    fn sql(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Ne => "<>",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Like => "LIKE",
            Op::IsNull => "IS NULL",
            Op::IsNotNull => "IS NOT NULL",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cond {
    pub column: String,
    pub op: Op,
    pub value: Value,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Where {
    pub conds: Vec<Cond>,
    pub or: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Sort {
    pub column: String,
    pub desc: bool,
}

fn push_param(params: &mut Vec<Value>, v: Value) -> usize {
    params.push(v);
    params.len()
}

/// `WHERE ...` clause (leading space) for `w`; empty when there are no conditions.
fn where_sql(d: Dialect, t: &Table, w: &Where, params: &mut Vec<Value>) -> String {
    if w.conds.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = w
        .conds
        .iter()
        .map(|c| {
            let col = quote_ident(&c.column);
            if matches!(c.op, Op::IsNull | Op::IsNotNull) {
                return format!("{col} {}", c.op.sql());
            }
            let rhs = match d {
                Dialect::Sqlite => d.ph(push_param(params, c.value.clone())),
                Dialect::Postgres => {
                    let n = push_param(params, Value::Object(Map::from_iter([(c.column.clone(), c.value.clone())])));
                    format!("(jsonb_populate_record(NULL::{}, {})).{col}", t.sql(), d.ph(n))
                }
            };
            format!("{col} {} {rhs}", c.op.sql())
        })
        .collect();
    format!(" WHERE {}", parts.join(if w.or { " OR " } else { " AND " }))
}

fn cols_sql(cols: impl Iterator<Item = impl AsRef<str>>) -> String {
    cols.map(|c| quote_ident(c.as_ref())).collect::<Vec<_>>().join(", ")
}

/// `INSERT` head shared by insert and upsert: sqlite binds one param per column, postgres one
/// JSON record.
fn insert_head(d: Dialect, t: &Table, row: &Map<String, Value>, params: &mut Vec<Value>) -> String {
    if row.is_empty() {
        return format!("INSERT INTO {} DEFAULT VALUES", t.sql());
    }
    let cols = cols_sql(row.keys());
    match d {
        Dialect::Sqlite => {
            let phs: Vec<String> = row.values().map(|v| d.ph(push_param(params, v.clone()))).collect();
            format!("INSERT INTO {} ({cols}) VALUES ({})", t.sql(), phs.join(", "))
        }
        Dialect::Postgres => {
            let n = push_param(params, Value::Object(row.clone()));
            format!(
                "INSERT INTO {} ({cols}) SELECT {cols} FROM jsonb_populate_record(NULL::{}, {})",
                t.sql(),
                t.sql(),
                d.ph(n)
            )
        }
    }
}

pub(crate) fn insert(d: Dialect, t: &Table, row: &Map<String, Value>) -> Stmt {
    let mut params = Vec::new();
    let sql = format!("{} RETURNING *", insert_head(d, t, row, &mut params));
    Stmt { sql, params }
}

/// Insert, or update the non-key columns when a row with the same `matching` values exists.
pub(crate) fn upsert(d: Dialect, t: &Table, row: &Map<String, Value>, matching: &[String]) -> Result<Stmt, String> {
    check_matching(row, matching)?;
    let mut params = Vec::new();
    let mut sql = insert_head(d, t, row, &mut params);
    let updates: Vec<String> = row
        .keys()
        .filter(|c| !matching.contains(c))
        .map(|c| format!("{0} = EXCLUDED.{0}", quote_ident(c)))
        .collect();
    sql.push_str(&format!(" ON CONFLICT ({}) ", cols_sql(matching.iter())));
    if updates.is_empty() {
        sql.push_str("DO NOTHING");
    } else {
        sql.push_str(&format!("DO UPDATE SET {}", updates.join(", ")));
    }
    sql.push_str(" RETURNING *");
    Ok(Stmt { sql, params })
}

fn check_matching(row: &Map<String, Value>, matching: &[String]) -> Result<(), String> {
    if matching.is_empty() {
        return Err("Select at least one column to match on".into());
    }
    if let Some(c) = matching.iter().find(|c| !row.contains_key(*c)) {
        return Err(format!("The matching column `{c}` has no value"));
    }
    Ok(())
}

/// `UPDATE` rows whose `matching` columns equal the given values; the other columns are set.
pub(crate) fn update(d: Dialect, t: &Table, row: &Map<String, Value>, matching: &[String]) -> Result<Stmt, String> {
    check_matching(row, matching)?;
    let set: Vec<&String> = row.keys().filter(|c| !matching.contains(c)).collect();
    if set.is_empty() {
        return Err("There is nothing to update besides the matching columns".into());
    }
    let mut params = Vec::new();
    let sql = match d {
        Dialect::Sqlite => {
            let sets: Vec<String> = set
                .iter()
                .map(|c| format!("{} = {}", quote_ident(c), d.ph(push_param(&mut params, row[*c].clone()))))
                .collect();
            let wh: Vec<String> = matching
                .iter()
                .map(|c| format!("{} = {}", quote_ident(c), d.ph(push_param(&mut params, row[c].clone()))))
                .collect();
            format!("UPDATE {} SET {} WHERE {} RETURNING *", t.sql(), sets.join(", "), wh.join(" AND "))
        }
        Dialect::Postgres => {
            let n = push_param(&mut params, Value::Object(row.clone()));
            let sets: Vec<String> = set.iter().map(|c| format!("{0} = r.{0}", quote_ident(c))).collect();
            let wh: Vec<String> = matching.iter().map(|c| format!("t.{0} = r.{0}", quote_ident(c))).collect();
            format!(
                "UPDATE {tbl} AS t SET {} FROM jsonb_populate_record(NULL::{tbl}, {}) AS r WHERE {} RETURNING t.*",
                sets.join(", "),
                d.ph(n),
                wh.join(" AND "),
                tbl = t.sql()
            )
        }
    };
    Ok(Stmt { sql, params })
}

pub(crate) fn select(
    d: Dialect,
    t: &Table,
    columns: &[String],
    w: &Where,
    sort: &[Sort],
    limit: Option<u64>,
) -> Stmt {
    let mut params = Vec::new();
    let cols = if columns.is_empty() { "*".to_owned() } else { cols_sql(columns.iter()) };
    let mut sql = format!("SELECT {cols} FROM {}", t.sql());
    sql.push_str(&where_sql(d, t, w, &mut params));
    if !sort.is_empty() {
        let order: Vec<String> = sort
            .iter()
            .map(|s| format!("{} {}", quote_ident(&s.column), if s.desc { "DESC" } else { "ASC" }))
            .collect();
        sql.push_str(&format!(" ORDER BY {}", order.join(", ")));
    }
    if let Some(n) = limit {
        sql.push_str(&format!(" LIMIT {n}"));
    }
    Stmt { sql, params }
}

/// Delete matching rows. Refuses an empty condition list: use `deleteTable` to wipe a table.
pub(crate) fn delete(d: Dialect, t: &Table, w: &Where) -> Result<Stmt, String> {
    if w.conds.is_empty() {
        return Err("Add at least one condition, or use the Delete Table operation".into());
    }
    let mut params = Vec::new();
    let sql = format!("DELETE FROM {}{}", t.sql(), where_sql(d, t, w, &mut params));
    Ok(Stmt { sql, params })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TableCommand {
    Truncate,
    Drop,
}

pub(crate) fn delete_table(d: Dialect, t: &Table, cmd: TableCommand) -> Stmt {
    let sql = match (cmd, d) {
        (TableCommand::Drop, _) => format!("DROP TABLE {}", t.sql()),
        (TableCommand::Truncate, Dialect::Postgres) => format!("TRUNCATE TABLE {}", t.sql()),
        // sqlite has no TRUNCATE
        (TableCommand::Truncate, Dialect::Sqlite) => format!("DELETE FROM {}", t.sql()),
    };
    Stmt { sql, params: Vec::new() }
}

// ---------------------------------------------------------------- node parameter readers

/// Query with `{{ expr }}` turned into bound parameters appended to `params`. A raw value
/// without the leading `=` is taken verbatim (braces and all).
pub(crate) fn bind_query(
    ctx: &NodeContext,
    raw: &Value,
    i: usize,
    d: Dialect,
    params: &mut Vec<Value>,
) -> Result<String, FlowError> {
    let Some(text) = raw.as_str() else {
        return Ok(display(raw));
    };
    let Some(tpl) = text.strip_prefix('=') else {
        return Ok(text.to_owned());
    };
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or_else(|| {
            crate::kit::node_err(ctx, "Unterminated `{{` in the query")
        })?;
        let expr = after[..end].trim();
        if expr.is_empty() {
            return Err(crate::kit::node_err(ctx, "Empty `{{ }}` in the query"));
        }
        let value = ctx.eval_param(&Value::String(format!("={{{{ {expr} }}}}")), i)?;
        out.push_str(&d.ph(push_param(params, value)));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `queryReplacement`: an array (expression result) or a comma separated string.
pub(crate) fn replacements(ctx: &NodeContext, i: usize) -> Result<Vec<Value>, FlowError> {
    Ok(match ctx.eval_param_at("options.queryReplacement", i)?.or(ctx.eval_param_at("queryReplacement", i)?) {
        Some(Value::Array(a)) => a,
        Some(Value::String(s)) if s.trim().is_empty() => Vec::new(),
        Some(Value::String(s)) => s.split(',').map(|p| Value::String(p.trim().to_owned())).collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![other],
    })
}

pub(crate) fn table(ctx: &NodeContext, i: usize, with_schema: bool) -> Result<Table, FlowError> {
    let name = ctx.eval_param_at("table", i)?.map(|v| rl_value(&v)).unwrap_or_default();
    if name.trim().is_empty() {
        return Err(crate::kit::node_err(ctx, "Parameter 'Table' is required"));
    }
    let schema = if with_schema {
        ctx.eval_param_at("schema", i)?.map(|v| rl_value(&v)).filter(|s| !s.is_empty()).or(Some("public".into()))
    } else {
        None
    };
    Ok(Table { schema, name })
}

/// Values to write and the columns that identify a row, from the `columns` parameter.
#[derive(Debug, Default)]
pub(crate) struct Mapping {
    pub row: Map<String, Value>,
    pub matching: Vec<String>,
}

fn names(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().map(display).filter(|s| !s.is_empty()).collect(),
        Value::String(s) => s.split(',').map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect(),
        _ => Vec::new(),
    }
}

/// n8n resource-mapper shape (`{mappingMode, value, matchingColumns}`), or a comma list of
/// item fields, or nothing (all fields of the input item).
pub(crate) fn mapping(ctx: &NodeContext, i: usize) -> Result<Mapping, FlowError> {
    let item = ctx.input.first().and_then(|p| p.get(i)).map(|it| it.json.clone()).unwrap_or_default();
    let top_matching = ctx.eval_param_at("matchingColumns", i)?.map(|v| names(&v)).unwrap_or_default();
    let Some(cols) = ctx
        .eval_param_at("columns", i)?
        .filter(|v| !v.is_null() && v.as_str().is_none_or(|s| !s.trim().is_empty()))
    else {
        return Ok(Mapping { row: item, matching: top_matching });
    };
    let mut m = match cols {
        Value::String(_) => {
            let row = names(&cols).into_iter().map(|c| { let v = item.get(&c).cloned().unwrap_or(Value::Null); (c, v) }).collect();
            Mapping { row, matching: Vec::new() }
        }
        Value::Object(m) => {
            let matching = m.get("matchingColumns").map(names).unwrap_or_default();
            let row = match (m.get("mappingMode").and_then(Value::as_str), m.get("value")) {
                (Some("autoMapInputData") | None, None | Some(Value::Null)) => item,
                (_, Some(Value::Object(v))) => v.clone(),
                _ => item,
            };
            Mapping { row, matching }
        }
        _ => Mapping { row: item, matching: Vec::new() },
    };
    if m.matching.is_empty() {
        m.matching = top_matching;
    }
    Ok(m)
}

/// A `json`-typed parameter may hold the object or its text.
fn json_param(ctx: &NodeContext, path: &str, i: usize) -> Result<Option<Value>, FlowError> {
    Ok(ctx.eval_param_at(path, i)?.map(|v| match v {
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::Null),
        other => other,
    }))
}

pub(crate) fn conditions(ctx: &NodeContext, i: usize) -> Result<Where, FlowError> {
    let mut w = Where::default();
    if let Some(Value::Array(vals)) = json_param(ctx, "where", i)?.and_then(|w| w.get("values").cloned()) {
        for v in vals {
            let column = v.get("column").map(rl_value).unwrap_or_default();
            if column.is_empty() {
                continue;
            }
            let cond = v.get("condition").and_then(Value::as_str).unwrap_or("equal");
            let op = Op::parse(cond)
                .ok_or_else(|| crate::kit::node_err(ctx, format!("Unsupported condition `{cond}`")))?;
            w.conds.push(Cond { column, op, value: v.get("value").cloned().unwrap_or(Value::Null) });
        }
    }
    w.or = ctx
        .eval_param_at("combineConditions", i)?
        .is_some_and(|v| display(&v).eq_ignore_ascii_case("or"));
    Ok(w)
}

pub(crate) fn sorting(ctx: &NodeContext, i: usize) -> Result<Vec<Sort>, FlowError> {
    let mut out = Vec::new();
    if let Some(Value::Array(vals)) = json_param(ctx, "sort", i)?.and_then(|w| w.get("values").cloned()) {
        for v in vals {
            let column = v.get("column").map(rl_value).unwrap_or_default();
            if !column.is_empty() {
                let desc = v.get("direction").is_some_and(|d| display(d).eq_ignore_ascii_case("desc"));
                out.push(Sort { column, desc });
            }
        }
    }
    Ok(out)
}

pub(crate) fn output_columns(ctx: &NodeContext, i: usize) -> Result<Vec<String>, FlowError> {
    let cols = ctx.eval_param_at("options.outputColumns", i)?.map(|v| names(&v)).unwrap_or_default();
    Ok(if cols.iter().any(|c| c == "*") { Vec::new() } else { cols })
}

/// Row limit for select: `None` when returning all.
pub(crate) fn limit(ctx: &NodeContext, i: usize) -> Result<Option<u64>, FlowError> {
    if matches!(ctx.eval_param_at("returnAll", i)?, Some(Value::Bool(true))) {
        return Ok(None);
    }
    Ok(Some(
        ctx.eval_param_at("limit", i)?
            .and_then(|v| v.as_u64().or_else(|| display(&v).parse().ok()))
            .filter(|n| *n > 0)
            .unwrap_or(50),
    ))
}

/// Operation and parameter set shared by the two SQL nodes. `schema` is Postgres only.
pub(crate) fn properties(d: Dialect) -> Vec<Prop> {
    const TABLE_OPS: &[&str] = &["insert", "update", "upsert", "select", "delete", "deleteTable"];
    let mut ps = vec![
        Prop::options(
            "operation",
            "Operation",
            vec![
                Choice::new("Execute Query", "executeQuery").action("Execute a SQL query"),
                Choice::new("Insert", "insert").action("Insert rows in a table"),
                Choice::new("Insert or Update", "upsert").action("Insert or update rows in a table"),
                Choice::new("Select", "select").action("Select rows from a table"),
                Choice::new("Update", "update").action("Update rows in a table"),
                Choice::new("Delete", "delete").action("Delete rows from a table"),
                Choice::new("Delete Table", "deleteTable").action("Truncate or drop a table"),
            ],
        ),
    ];
    if d == Dialect::Postgres {
        ps.push(Prop::string("schema", "Schema").default("public").show("operation", TABLE_OPS));
    }
    ps.extend([
        Prop::string("table", "Table").required().show("operation", TABLE_OPS),
        Prop::string("query", "Query")
            .required()
            .type_option("rows", 5)
            .placeholder("SELECT * FROM users WHERE id = {{ $json.id }}")
            .hint("`{{ }}` expressions become bound parameters, never part of the SQL text")
            .show("operation", &["executeQuery"]),
        Prop::string("columns", "Columns")
            .description("Comma-separated fields of the input item to write. Empty: all fields")
            .show("operation", &["insert", "update", "upsert"]),
        Prop::string("matchingColumns", "Columns to Match On")
            .description("Comma-separated columns that identify the row")
            .show("operation", &["update", "upsert"]),
        Prop::json("where", "Where")
            .default(serde_json::json!({ "values": [] }))
            .description("`{values: [{column, condition, value}]}`; conditions: equal, notEqual, larger, largerEqual, smaller, smallerEqual, like, isNull, isNotNull")
            .show("operation", &["select", "delete"]),
        Prop::options("combineConditions", "Combine Conditions", vec![Choice::new("AND", "AND"), Choice::new("OR", "OR")])
            .show("operation", &["select", "delete"]),
        Prop::json("sort", "Sort")
            .default(serde_json::json!({ "values": [] }))
            .description("`{values: [{column, direction}]}`, direction ASC or DESC")
            .show("operation", &["select"]),
        Prop::options(
            "deleteCommand",
            "Command",
            vec![Choice::new("Truncate", "truncate"), Choice::new("Drop", "drop")],
        )
        .show("operation", &["deleteTable"]),
    ]);
    ps.extend(return_all_and_limit(None, &["select"]));
    ps.push(Prop::collection(
        "options",
        "Options",
        vec![
            Prop::string("queryReplacement", "Query Parameters")
                .description("Values for the numbered placeholders of the query, comma-separated or an array expression"),
            Prop::string("outputColumns", "Output Columns").description("Comma-separated columns to return from Select; `*` for all"),
        ],
    ));
    ps
}

/// Statement for one input item, for every operation but `executeQuery`.
pub(crate) fn plan_structured(ctx: &NodeContext, op: &str, d: Dialect, i: usize) -> Result<Stmt, FlowError> {
    let bad = |m: String| crate::kit::node_err(ctx, m);
    let t = table(ctx, i, d == Dialect::Postgres)?;
    match op {
        "insert" => Ok(insert(d, &t, &mapping(ctx, i)?.row)),
        "update" => {
            let m = mapping(ctx, i)?;
            update(d, &t, &m.row, &m.matching).map_err(bad)
        }
        "upsert" => {
            let m = mapping(ctx, i)?;
            upsert(d, &t, &m.row, &m.matching).map_err(bad)
        }
        "select" => Ok(select(d, &t, &output_columns(ctx, i)?, &conditions(ctx, i)?, &sorting(ctx, i)?, limit(ctx, i)?)),
        "delete" => delete(d, &t, &conditions(ctx, i)?).map_err(bad),
        "deleteTable" => {
            let cmd = match ctx.eval_param_at("deleteCommand", i)?.map(|v| display(&v)).as_deref() {
                Some("drop") => TableCommand::Drop,
                _ => TableCommand::Truncate,
            };
            Ok(delete_table(d, &t, cmd))
        }
        other => Err(bad(format!("Unsupported operation `{other}`"))),
    }
}

/// `executeQuery` statement: `{{ }}` bound after any `queryReplacement` values.
pub(crate) fn plan_query(ctx: &NodeContext, d: Dialect, i: usize) -> Result<Stmt, FlowError> {
    let mut params = replacements(ctx, i)?;
    let raw = ctx.node.parameters.get("query").cloned().unwrap_or(Value::Null);
    let sql = bind_query(ctx, &raw, i, d, &mut params)?;
    if sql.trim().is_empty() {
        return Err(crate::kit::node_err(ctx, "Parameter 'Query' is required"));
    }
    Ok(Stmt { sql, params })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn t() -> Table {
        Table { schema: Some("public".into()), name: "us\"ers".into() }
    }

    fn row() -> Map<String, Value> {
        json!({"id": 1, "name": "ann"}).as_object().unwrap().clone()
    }

    #[test]
    fn identifiers_are_quoted() {
        assert_eq!(t().sql(), "\"public\".\"us\"\"ers\"");
        assert_eq!(Table { schema: None, name: "a".into() }.sql(), "\"a\"");
    }

    #[test]
    fn insert_pg_goes_through_a_json_record() {
        let s = insert(Dialect::Postgres, &t(), &row());
        assert_eq!(
            s.sql,
            "INSERT INTO \"public\".\"us\"\"ers\" (\"id\", \"name\") SELECT \"id\", \"name\" FROM \
             jsonb_populate_record(NULL::\"public\".\"us\"\"ers\", $1) RETURNING *"
        );
        assert_eq!(s.params, vec![json!({"id": 1, "name": "ann"})]);
    }

    #[test]
    fn insert_sqlite_binds_each_value() {
        let s = insert(Dialect::Sqlite, &Table { schema: None, name: "u".into() }, &row());
        assert_eq!(s.sql, "INSERT INTO \"u\" (\"id\", \"name\") VALUES (?1, ?2) RETURNING *");
        assert_eq!(s.params, vec![json!(1), json!("ann")]);
        // a hostile value stays a parameter
        let mut r = Map::new();
        r.insert("name".into(), json!("x'); DROP TABLE u;--"));
        assert!(!insert(Dialect::Sqlite, &Table { schema: None, name: "u".into() }, &r).sql.contains("DROP"));
    }

    #[test]
    fn update_and_upsert_need_matching_columns() {
        assert!(update(Dialect::Postgres, &t(), &row(), &[]).is_err());
        assert!(update(Dialect::Postgres, &t(), &row(), &["nope".into()]).is_err());
        assert!(update(Dialect::Postgres, &t(), &row(), &["id".into(), "name".into()]).is_err());
        let s = update(Dialect::Postgres, &t(), &row(), &["id".into()]).unwrap();
        assert_eq!(
            s.sql,
            "UPDATE \"public\".\"us\"\"ers\" AS t SET \"name\" = r.\"name\" FROM \
             jsonb_populate_record(NULL::\"public\".\"us\"\"ers\", $1) AS r WHERE t.\"id\" = r.\"id\" RETURNING t.*"
        );
        let s = update(Dialect::Sqlite, &t(), &row(), &["id".into()]).unwrap();
        assert!(s.sql.ends_with("SET \"name\" = ?1 WHERE \"id\" = ?2 RETURNING *"), "{}", s.sql);
        assert_eq!(s.params, vec![json!("ann"), json!(1)]);
        let s = upsert(Dialect::Postgres, &t(), &row(), &["id".into()]).unwrap();
        assert!(s.sql.ends_with("ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\" RETURNING *"), "{}", s.sql);
    }

    #[test]
    fn select_builds_where_sort_limit() {
        let w = Where {
            conds: vec![
                Cond { column: "age".into(), op: Op::Ge, value: json!(18) },
                Cond { column: "name".into(), op: Op::IsNotNull, value: Value::Null },
            ],
            or: false,
        };
        let sort = [Sort { column: "age".into(), desc: true }];
        let s = select(Dialect::Postgres, &t(), &["id".into()], &w, &sort, Some(10));
        assert_eq!(
            s.sql,
            "SELECT \"id\" FROM \"public\".\"us\"\"ers\" WHERE \"age\" >= \
             (jsonb_populate_record(NULL::\"public\".\"us\"\"ers\", $1)).\"age\" AND \"name\" IS NOT NULL \
             ORDER BY \"age\" DESC LIMIT 10"
        );
        assert_eq!(s.params, vec![json!({"age": 18})]);
        let s = select(Dialect::Sqlite, &Table { schema: None, name: "u".into() }, &[], &w, &[], None);
        assert_eq!(s.sql, "SELECT * FROM \"u\" WHERE \"age\" >= ?1 AND \"name\" IS NOT NULL");
    }

    #[test]
    fn delete_refuses_to_wipe() {
        assert!(delete(Dialect::Sqlite, &t(), &Where::default()).is_err());
        let w = Where { conds: vec![Cond { column: "id".into(), op: Op::Eq, value: json!(3) }], or: true };
        assert_eq!(delete(Dialect::Sqlite, &Table { schema: None, name: "u".into() }, &w).unwrap().sql, "DELETE FROM \"u\" WHERE \"id\" = ?1");
        assert_eq!(delete_table(Dialect::Sqlite, &t(), TableCommand::Truncate).sql, "DELETE FROM \"public\".\"us\"\"ers\"");
        assert_eq!(delete_table(Dialect::Postgres, &t(), TableCommand::Truncate).sql, "TRUNCATE TABLE \"public\".\"us\"\"ers\"");
    }
}
