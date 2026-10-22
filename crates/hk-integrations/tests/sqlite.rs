//! SQLite node against a temp database file: every operation, and expressions as bound params.
#![cfg(feature = "int-sqlite")]

mod common;

use std::sync::Arc;

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::{CredentialTypes, register_integrations};
use serde_json::{Value, json};

use common::Creds;

struct Db(std::path::PathBuf);

impl Db {
    fn new(tag: &str) -> Self {
        Self(std::env::temp_dir().join(format!("hk-sqlite-test-{tag}-{}.db", std::process::id())))
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn setup() -> NodeRegistry {
    let mut nodes = NodeRegistry::new();
    register_integrations(&mut nodes, &CredentialTypes::new()).unwrap();
    nodes
}

async fn run(db: &Db, params: Value, input: Vec<Value>) -> Result<Vec<Item>, hk_flow::FlowError> {
    let nodes = setup();
    let node: Node = serde_json::from_value(json!({
        "name": "SQLite", "type": "hokkabaz.sqlite", "typeVersion": 1, "position": [0, 0],
        "parameters": params,
        "credentials": {"sqliteDb": {"id": "c1", "name": "db"}}
    }))
    .unwrap();
    let items = input.into_iter().map(Item::from_json).collect();
    let mut c = NodeContext::new(node, vec![items], 0);
    c.hooks.credentials = Some(Arc::new(Creds(json!({"path": db.0.to_str().unwrap()}))));
    nodes.latest("hokkabaz.sqlite").unwrap().execute(&c).await.map(|mut o| o.remove(0))
}

fn rows(items: &[Item]) -> Vec<Value> {
    items.iter().map(|i| Value::Object(i.json.clone())).collect()
}

#[tokio::test]
async fn crud_roundtrip() {
    let db = Db::new("crud");
    run(
        &db,
        json!({"operation": "executeQuery", "query": "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)"}),
        vec![json!({})],
    )
    .await
    .unwrap();

    // insert: item fields become columns, RETURNING * gives the row back
    let out = run(&db, json!({"operation": "insert", "table": "users"}), vec![json!({"id": 1, "name": "ann", "age": 30}), json!({"id": 2, "name": "bob", "age": 17})])
        .await
        .unwrap();
    assert_eq!(rows(&out), vec![json!({"id": 1, "name": "ann", "age": 30}), json!({"id": 2, "name": "bob", "age": 17})]);

    // select with where + sort
    let params = json!({
        "operation": "select", "table": "users", "returnAll": true,
        "where": {"values": [{"column": "age", "condition": "larger", "value": 18}]},
        "sort": {"values": [{"column": "id", "direction": "DESC"}]}
    });
    let out = run(&db, params, vec![json!({})]).await.unwrap();
    assert_eq!(rows(&out), vec![json!({"id": 1, "name": "ann", "age": 30})]);

    // update by matching column
    let params = json!({"operation": "update", "table": "users", "matchingColumns": "id"});
    let out = run(&db, params, vec![json!({"id": 2, "name": "robert", "age": 18})]).await.unwrap();
    assert_eq!(rows(&out), vec![json!({"id": 2, "name": "robert", "age": 18})]);

    // upsert: existing id updates, new id inserts
    let params = json!({"operation": "upsert", "table": "users", "matchingColumns": "id"});
    run(&db, params, vec![json!({"id": 1, "name": "anna", "age": 31}), json!({"id": 3, "name": "cy", "age": 5})]).await.unwrap();
    let out = run(&db, json!({"operation": "select", "table": "users", "returnAll": true, "options": {"outputColumns": "name"}}), vec![json!({})])
        .await
        .unwrap();
    assert_eq!(rows(&out), vec![json!({"name": "anna"}), json!({"name": "robert"}), json!({"name": "cy"})]);

    // delete needs a condition
    let params = json!({"operation": "delete", "table": "users"});
    assert!(run(&db, params, vec![json!({})]).await.is_err());
    let params = json!({"operation": "delete", "table": "users", "where": {"values": [{"column": "id", "condition": "smaller", "value": 3}]}});
    let out = run(&db, params, vec![json!({})]).await.unwrap();
    assert_eq!(out[0].json["changes"], 2);

    let out = run(&db, json!({"operation": "deleteTable", "table": "users", "deleteCommand": "drop"}), vec![json!({})]).await.unwrap();
    assert_eq!(out[0].json["success"], true);
    assert!(run(&db, json!({"operation": "select", "table": "users"}), vec![json!({})]).await.is_err());
}

#[tokio::test]
async fn expressions_in_queries_are_bound_not_concatenated() {
    let db = Db::new("bind");
    run(&db, json!({"operation": "executeQuery", "query": "CREATE TABLE t (name TEXT)"}), vec![json!({})]).await.unwrap();
    let hostile = "x'); DROP TABLE t;--";
    let params = json!({"operation": "executeQuery", "query": "=INSERT INTO t (name) VALUES ({{ $json.name }}) RETURNING name"});
    let out = run(&db, params, vec![json!({"name": hostile}), json!({"name": "ok"})]).await.unwrap();
    assert_eq!(rows(&out), vec![json!({"name": hostile}), json!({"name": "ok"})]);
    // positional values from queryReplacement come first
    let params = json!({"operation": "executeQuery", "query": "=SELECT name FROM t WHERE name = ?1 OR name = {{ $json.name }}", "options": {"queryReplacement": "ok"}});
    let out = run(&db, params, vec![json!({"name": hostile})]).await.unwrap();
    assert_eq!(out.len(), 2);
    // the table is still there
    let out = run(&db, json!({"operation": "executeQuery", "query": "SELECT count(*) AS n FROM t"}), vec![json!({})]).await.unwrap();
    assert_eq!(out[0].json["n"], 2);
}

#[tokio::test]
async fn query_errors_surface() {
    let db = Db::new("err");
    let out = run(&db, json!({"operation": "executeQuery", "query": "SELECT * FROM missing"}), vec![json!({})]).await;
    assert!(out.unwrap_err().to_string().contains("no such table"));
}
