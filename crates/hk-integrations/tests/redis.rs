//! Redis node against a tiny in-process RESP2 server.
#![cfg(feature = "int-redis")]

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hk_flow::{Item, Node, NodeContext, NodeRegistry};
use hk_integrations::{CredentialTypes, register_integrations};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use common::Creds;

#[derive(Default)]
struct State {
    strings: BTreeMap<String, String>,
    lists: BTreeMap<String, Vec<String>>,
    log: Vec<Vec<String>>,
}

/// Minimal Redis: enough commands for the node, no expiry, no pub/sub.
async fn fake_redis() -> (u16, Arc<Mutex<State>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(Mutex::new(State::default()));
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { return };
            let st = st.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut r = BufReader::new(r);
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let n: usize = line.trim()[1..].parse().unwrap();
                    let mut args = Vec::new();
                    for _ in 0..n {
                        let mut l = String::new();
                        r.read_line(&mut l).await.unwrap();
                        let len: usize = l.trim()[1..].parse().unwrap();
                        let mut buf = vec![0u8; len + 2];
                        r.read_exact(&mut buf).await.unwrap();
                        buf.truncate(len);
                        args.push(String::from_utf8(buf).unwrap());
                    }
                    let reply = answer(&mut st.lock().unwrap(), &args);
                    w.write_all(reply.as_bytes()).await.unwrap();
                }
            });
        }
    });
    (port, state)
}

fn bulk(s: &str) -> String {
    format!("${}\r\n{s}\r\n", s.len())
}

fn nil() -> String {
    "$-1\r\n".into()
}

fn arr(items: &[String]) -> String {
    format!("*{}\r\n{}", items.len(), items.concat())
}

fn answer(st: &mut State, a: &[String]) -> String {
    st.log.push(a.to_vec());
    match a[0].to_uppercase().as_str() {
        "AUTH" => if a.last().unwrap() == "secret" { "+OK\r\n".into() } else { "-WRONGPASS invalid password\r\n".into() },
        "SELECT" | "EXPIRE" => "+OK\r\n".into(),
        "SET" => { st.strings.insert(a[1].clone(), a[2].clone()); "+OK\r\n".into() }
        "GET" => st.strings.get(&a[1]).map_or_else(nil, |v| bulk(v)),
        "DEL" => format!(":{}\r\n", i32::from(st.strings.remove(&a[1]).is_some())),
        "INCR" => {
            let n = st.strings.get(&a[1]).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0) + 1;
            st.strings.insert(a[1].clone(), n.to_string());
            format!(":{n}\r\n")
        }
        "TYPE" => format!("+{}\r\n", if st.strings.contains_key(&a[1]) { "string" } else if st.lists.contains_key(&a[1]) { "list" } else { "none" }),
        "SCAN" => {
            // one page: every string key matching a trailing-* glob
            let prefix = a[3].trim_end_matches('*');
            let keys: Vec<String> = st.strings.keys().filter(|k| k.starts_with(prefix)).map(|k| bulk(k)).collect();
            format!("*2\r\n{}{}", bulk("0"), arr(&keys))
        }
        "LRANGE" => arr(&st.lists.get(&a[1]).cloned().unwrap_or_default().iter().map(|v| bulk(v)).collect::<Vec<_>>()),
        "LPUSH" => { let l = st.lists.entry(a[1].clone()).or_default(); l.insert(0, a[2].clone()); format!(":{}\r\n", l.len()) }
        "RPUSH" => { let l = st.lists.entry(a[1].clone()).or_default(); l.push(a[2].clone()); format!(":{}\r\n", l.len()) }
        "LPOP" => st.lists.get_mut(&a[1]).filter(|l| !l.is_empty()).map_or_else(nil, |l| bulk(&l.remove(0))),
        "RPOP" => st.lists.get_mut(&a[1]).and_then(Vec::pop).map_or_else(nil, |v| bulk(&v)),
        "LLEN" => format!(":{}\r\n", st.lists.get(&a[1]).map_or(0, Vec::len)),
        "PUBLISH" => ":2\r\n".into(),
        "INFO" => bulk("# Server\r\nredis_version:7.0.0\r\nconnected_clients:3\r\n"),
        other => format!("-ERR unknown command '{other}'\r\n"),
    }
}

fn setup() -> NodeRegistry {
    let mut nodes = NodeRegistry::new();
    register_integrations(&mut nodes, &CredentialTypes::new()).unwrap();
    nodes
}

async fn run(port: u16, params: Value, input: Vec<Value>) -> Result<Vec<Item>, hk_flow::FlowError> {
    let nodes = setup();
    let node: Node = serde_json::from_value(json!({
        "name": "Redis", "type": "n8n-nodes-base.redis", "typeVersion": 1, "position": [0, 0],
        "parameters": params,
        "credentials": {"redis": {"id": "c1", "name": "r"}}
    }))
    .unwrap();
    let items = input.into_iter().map(Item::from_json).collect();
    let mut c = NodeContext::new(node, vec![items], 0);
    let cred = json!({"host": "127.0.0.1", "port": port, "password": "secret", "database": 2});
    c.hooks.credentials = Some(Arc::new(Creds(cred)));
    nodes.latest("n8n-nodes-base.redis").unwrap().execute(&c).await.map(|mut o| o.remove(0))
}

#[tokio::test]
async fn set_get_incr_delete_with_auth_and_select() {
    let (port, st) = fake_redis().await;
    run(port, json!({"operation": "set", "key": "user", "value": "{\"a\":1}"}), vec![json!({"x": 1})]).await.unwrap();
    let out = run(port, json!({"operation": "get", "key": "user", "propertyName": "v"}), vec![json!({})]).await.unwrap();
    assert_eq!(out[0].json["v"], json!({"a": 1}));
    let out = run(port, json!({"operation": "incr", "key": "n", "expire": true, "ttl": 30}), vec![json!({})]).await.unwrap();
    assert_eq!(out[0].json["n"], 1);
    run(port, json!({"operation": "delete", "key": "user"}), vec![json!({})]).await.unwrap();
    let out = run(port, json!({"operation": "get", "key": "user"}), vec![json!({})]).await.unwrap();
    assert_eq!(out[0].json["propertyName"], Value::Null);
    let log = st.lock().unwrap().log.clone();
    assert_eq!(log[0], ["AUTH", "secret"]);
    assert_eq!(log[1], ["SELECT", "2"]);
    assert!(log.iter().any(|c| c == &["EXPIRE", "n", "30"]));
}

#[tokio::test]
async fn keys_push_pop_publish_info() {
    let (port, _st) = fake_redis().await;
    for k in ["app:1", "app:2", "other"] {
        run(port, json!({"operation": "set", "key": k, "value": k}), vec![json!({})]).await.unwrap();
    }
    let out = run(port, json!({"operation": "keys", "keyPattern": "app:*", "getValues": false}), vec![json!({})]).await.unwrap();
    assert_eq!(out[0].json["app:*"], json!(["app:1", "app:2"]));
    let out = run(port, json!({"operation": "keys", "keyPattern": "app:*", "getValues": true}), vec![json!({})]).await.unwrap();
    assert_eq!(Value::Object(out[0].json.clone()), json!({"app:1": "app:1", "app:2": "app:2"}));

    run(port, json!({"operation": "push", "list": "q", "messageData": "a"}), vec![json!({})]).await.unwrap();
    run(port, json!({"operation": "push", "list": "q", "messageData": "b", "tail": true}), vec![json!({})]).await.unwrap();
    let out = run(port, json!({"operation": "llen", "list": "q"}), vec![json!({})]).await.unwrap();
    assert_eq!(out[0].json["q"], 2);
    let out = run(port, json!({"operation": "pop", "list": "q", "tail": true, "propertyName": "p"}), vec![json!({})]).await.unwrap();
    assert_eq!(out[0].json["p"], "b");
    let out = run(port, json!({"operation": "publish", "channel": "c", "messageData": "hi"}), vec![json!({})]).await.unwrap();
    assert_eq!(out[0].json["receivers"], 2);
    let out = run(port, json!({"operation": "info"}), vec![json!({})]).await.unwrap();
    assert_eq!(out[0].json["connected_clients"], 3);
}

#[tokio::test]
async fn server_errors_surface() {
    let (port, _st) = fake_redis().await;
    // the fake rejects every command it does not know
    let err = run(port, json!({"operation": "get", "key": ""}), vec![json!({})]).await.unwrap_err();
    assert!(err.to_string().contains("'Key' is required"), "{err}");
}
