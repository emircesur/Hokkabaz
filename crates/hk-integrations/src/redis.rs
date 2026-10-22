//! Redis node (`n8n-nodes-base.redis`): delete, get, incr, info, keys, llen, pop, publish,
//! push, set. The Redis Trigger (pub/sub) is not implemented.
//!
//! A small RESP2 client over `tokio::net::TcpStream` (optionally wrapped in tokio-rustls with
//! ring, so no openssl and no aws-lc on android). The command set is tiny and fixed, which
//! makes this lighter and easier to fake in tests than the `redis` crate and its TLS matrix.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop};
use crate::kit::{self, node_err};
use crate::template::display;

pub const NODE_TYPE: &str = "n8n-nodes-base.redis";
pub const CREDENTIAL: &str = "redis";

const TIMEOUT: Duration = Duration::from_secs(30);
/// `keys` refuses to buffer more than this many names.
const MAX_KEYS: usize = 50_000;
/// Longest reply the client accepts (bulk string bytes or array length).
const MAX_REPLY: usize = 64 * 1024 * 1024;

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "Redis")
        .documentation("https://redis.io/docs/latest/commands/auth/")
        .property(Prop::string("password", "Password").password())
        .property(Prop::string("host", "Host").default("localhost"))
        .property(Prop::number("port", "Port").default(6379))
        .property(Prop::number("database", "Database Number").default(0))
        .property(Prop::boolean("ssl", "SSL").description("Connect over TLS (verified against the public web PKI)"))
        .property(Prop::string("user", "User").description("ACL user; leave empty for the default user"))
}

fn description() -> NodeTypeDescription {
    let ops = [
        ("Delete", "delete", "Delete a key from Redis"),
        ("Get", "get", "Get the value of a key from Redis"),
        ("Increment", "incr", "Atomically increment a key by 1"),
        ("Info", "info", "Return generic information about the Redis instance"),
        ("Keys", "keys", "Return all keys matching a pattern"),
        ("List Length", "llen", "Return the length of a list"),
        ("Pop", "pop", "Pop data from a Redis list"),
        ("Publish", "publish", "Publish a message to a channel"),
        ("Push", "push", "Push data to a Redis list"),
        ("Set", "set", "Set the value of a key in Redis"),
    ];
    let key_types = || {
        vec![
            Choice::new("Automatic", "automatic"),
            Choice::new("Hash", "hash"),
            Choice::new("List", "list"),
            Choice::new("Sets", "sets"),
            Choice::new("String", "string"),
        ]
    };
    let mut d = NodeBuilder::new(NODE_TYPE, "Redis")
        .description("Get, send and update data in Redis")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:database")
        .color("#ff4500")
        .credential(CREDENTIAL, true)
        .property(
            Prop::options("operation", "Operation", ops.iter().map(|(n, v, a)| Choice::new(n, *v).action(a)).collect())
                // n8n's default; the first choice (delete) would be a risky one
                .default("info"),
        )
        .property(Prop::string("key", "Key").required().show("operation", &["delete", "get", "incr", "set"]))
        .property(
            Prop::string("propertyName", "Name")
                .default("propertyName")
                .description("Name of the output property to write the value to")
                .show("operation", &["get", "pop"]),
        )
        .property(Prop::options("keyType", "Key Type", key_types()).show("operation", &["get", "set"]))
        .property(Prop::string("value", "Value").show("operation", &["set"]))
        .property(Prop::boolean("expire", "Expire").description("Whether to set a timeout on the key").show("operation", &["set", "incr"]))
        .property(Prop::number("ttl", "TTL").default(60).min(1.0).description("Seconds").show("operation", &["set", "incr"]).show_bool("expire", true))
        .property(Prop::string("keyPattern", "Key Pattern").default("*").required().show("operation", &["keys"]))
        .property(Prop::boolean("getValues", "Get Values").default(true).show("operation", &["keys"]))
        .property(Prop::string("list", "List").required().show("operation", &["llen", "pop", "push"]))
        .property(Prop::string("messageData", "Data").required().show("operation", &["push"]))
        .property(Prop::boolean("tail", "Tail").description("Whether to push/pop at the tail instead of the head").show("operation", &["pop", "push"]))
        .property(Prop::string("channel", "Channel").required().show("operation", &["publish"]))
        .property(Prop::string("messageData", "Data").required().show("operation", &["publish"]))
        .build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

// ------------------------------------------------------------------------------- RESP

#[derive(Debug, Clone, PartialEq)]
enum Resp {
    Simple(String),
    Int(i64),
    Bulk(Option<Vec<u8>>),
    Array(Option<Vec<Resp>>),
}

impl Resp {
    fn text(&self) -> Option<String> {
        match self {
            Resp::Simple(s) => Some(s.clone()),
            Resp::Bulk(Some(b)) => Some(String::from_utf8_lossy(b).into_owned()),
            Resp::Int(n) => Some(n.to_string()),
            _ => None,
        }
    }

    fn int(&self) -> i64 {
        match self {
            Resp::Int(n) => *n,
            other => other.text().and_then(|t| t.parse().ok()).unwrap_or(0),
        }
    }

    fn items(&self) -> &[Resp] {
        match self {
            Resp::Array(Some(a)) => a,
            _ => &[],
        }
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

struct Conn {
    io: BufReader<Box<dyn Io>>,
}

type Reply<'a> = Pin<Box<dyn Future<Output = Result<Resp, String>> + Send + 'a>>;

impl Conn {
    async fn open(host: &str, port: u16, tls: bool) -> Result<Self, String> {
        let tcp = tokio::time::timeout(TIMEOUT, TcpStream::connect((host, port)))
            .await
            .map_err(|_| "connection timed out".to_owned())?
            .map_err(|e| e.to_string())?;
        let io: Box<dyn Io> = if tls {
            let cfg = crate::tls::client_config(false)?;
            let name = tokio_rustls::rustls::pki_types::ServerName::try_from(host.to_owned())
                .map_err(|_| "the host is not a valid TLS server name".to_owned())?;
            let s = tokio_rustls::TlsConnector::from(cfg).connect(name, tcp).await.map_err(|e| e.to_string())?;
            Box::new(s)
        } else {
            Box::new(tcp)
        };
        Ok(Self { io: BufReader::new(io) })
    }

    /// Send one command and read its reply; error replies become `Err`.
    async fn cmd(&mut self, args: &[&[u8]]) -> Result<Resp, String> {
        let mut buf = format!("*{}\r\n", args.len()).into_bytes();
        for a in args {
            buf.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
            buf.extend_from_slice(a);
            buf.extend_from_slice(b"\r\n");
        }
        tokio::time::timeout(TIMEOUT, async {
            self.io.get_mut().write_all(&buf).await.map_err(|e| e.to_string())?;
            self.io.get_mut().flush().await.map_err(|e| e.to_string())?;
            self.read().await
        })
        .await
        .map_err(|_| "the server did not answer in time".to_owned())?
    }

    fn read(&mut self) -> Reply<'_> {
        Box::pin(async move {
            let mut line = String::new();
            if self.io.read_line(&mut line).await.map_err(|e| e.to_string())? == 0 {
                return Err("the server closed the connection".into());
            }
            let line = line.trim_end_matches(['\r', '\n']);
            let (kind, rest) = line.split_at_checked(1).ok_or("empty reply")?;
            let num = || rest.parse::<i64>().map_err(|_| format!("bad length `{rest}`"));
            match kind {
                "+" => Ok(Resp::Simple(rest.to_owned())),
                "-" => Err(rest.to_owned()),
                ":" => Ok(Resp::Int(num()?)),
                "$" => {
                    let n = num()?;
                    if n < 0 {
                        return Ok(Resp::Bulk(None));
                    }
                    let n = usize::try_from(n).map_err(|_| "bad length")?;
                    if n > MAX_REPLY {
                        return Err("reply too large".into());
                    }
                    let mut data = vec![0u8; n + 2];
                    self.io.read_exact(&mut data).await.map_err(|e| e.to_string())?;
                    data.truncate(n);
                    Ok(Resp::Bulk(Some(data)))
                }
                "*" => {
                    let n = num()?;
                    if n < 0 {
                        return Ok(Resp::Array(None));
                    }
                    let n = usize::try_from(n).map_err(|_| "bad length")?;
                    if n > MAX_REPLY {
                        return Err("reply too large".into());
                    }
                    let mut out = Vec::with_capacity(n.min(1024));
                    for _ in 0..n {
                        out.push(self.read().await?);
                    }
                    Ok(Resp::Array(Some(out)))
                }
                other => Err(format!("unexpected reply type `{other}`")),
            }
        })
    }

    async fn call(&mut self, args: &[&str]) -> Result<Resp, String> {
        let raw: Vec<&[u8]> = args.iter().map(|a| a.as_bytes()).collect();
        self.cmd(&raw).await
    }
}

/// Redis values are strings; n8n parses them as JSON when it can.
fn parse_value(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_owned()))
}

fn bulk_value(r: &Resp) -> Value {
    r.text().map_or(Value::Null, Value::String)
}

/// `INFO` text -> object (numbers where they parse).
fn parse_info(text: &str) -> Map<String, Value> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| {
            let v = v.trim();
            let val = v
                .parse::<i64>()
                .map(|n| json!(n))
                .or_else(|_| v.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number).ok_or(()))
                .unwrap_or_else(|()| Value::String(v.to_owned()));
            (k.to_owned(), val)
        })
        .collect()
}

// ------------------------------------------------------------------------------- node

#[derive(Debug)]
struct RedisNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

fn out_item(name: &str, value: Value) -> Vec<Item> {
    vec![Item::from_json(json!({ name: value }))]
}

impl RedisNode {
    fn s(&self, ctx: &NodeContext, name: &str, i: usize) -> Result<String, FlowError> {
        kit::str_param(ctx, &self.desc, name, i)
    }

    fn required(&self, ctx: &NodeContext, name: &str, label: &str, i: usize) -> Result<String, FlowError> {
        let v = self.s(ctx, name, i)?;
        if v.is_empty() {
            return Err(node_err(ctx, format!("Parameter '{label}' is required")));
        }
        Ok(v)
    }

    async fn expire_if(&self, ctx: &NodeContext, c: &mut Conn, key: &str, i: usize) -> Result<(), FlowError> {
        if kit::bool_param(ctx, &self.desc, "expire", i)? {
            let ttl = kit::param(ctx, &self.desc, "ttl", i)?.map(|v| display(&v)).unwrap_or_else(|| "60".into());
            c.call(&["EXPIRE", key, &ttl]).await.map_err(|m| kit::err(&ctx.node.name, m))?;
        }
        Ok(())
    }

    async fn run_item(&self, ctx: &NodeContext, c: &mut Conn, i: usize) -> Result<Vec<Item>, FlowError> {
        let node = ctx.node.name.clone();
        let e = |m: String| kit::err(&node, m);
        let input = ctx.input.first().and_then(|p| p.get(i)).cloned().unwrap_or_default();
        match self.s(ctx, "operation", i)?.as_str() {
            "delete" => {
                let key = self.required(ctx, "key", "Key", i)?;
                c.call(&["DEL", &key]).await.map_err(e)?;
                Ok(vec![input])
            }
            "get" => {
                let key = self.required(ctx, "key", "Key", i)?;
                let prop = self.s(ctx, "propertyName", i)?;
                let prop = if prop.is_empty() { "propertyName".to_owned() } else { prop };
                let mut ty = self.s(ctx, "keyType", i)?;
                if ty.is_empty() || ty == "automatic" {
                    ty = match c.call(&["TYPE", &key]).await.map_err(e)?.text().as_deref() {
                        Some("hash") => "hash",
                        Some("list") => "list",
                        Some("set") => "sets",
                        Some("none") => "none",
                        _ => "string",
                    }
                    .into();
                }
                let value = match ty.as_str() {
                    "none" => Value::Null,
                    "hash" => {
                        let r = c.call(&["HGETALL", &key]).await.map_err(e)?;
                        Value::Object(r.items().chunks(2).filter(|p| p.len() == 2).map(|p| (p[0].text().unwrap_or_default(), bulk_value(&p[1]))).collect())
                    }
                    "list" => Value::Array(c.call(&["LRANGE", &key, "0", "-1"]).await.map_err(e)?.items().iter().map(bulk_value).collect()),
                    "sets" => Value::Array(c.call(&["SMEMBERS", &key]).await.map_err(e)?.items().iter().map(bulk_value).collect()),
                    _ => c.call(&["GET", &key]).await.map_err(e)?.text().map_or(Value::Null, |t| parse_value(&t)),
                };
                Ok(out_item(&prop, value))
            }
            "incr" => {
                let key = self.required(ctx, "key", "Key", i)?;
                let n = c.call(&["INCR", &key]).await.map_err(e)?.int();
                self.expire_if(ctx, c, &key, i).await?;
                Ok(out_item(&key, json!(n)))
            }
            "info" => {
                let r = c.call(&["INFO"]).await.map_err(e)?;
                Ok(vec![Item::new(parse_info(&r.text().unwrap_or_default()))])
            }
            "keys" => {
                let pattern = self.required(ctx, "keyPattern", "Key Pattern", i)?;
                let mut seen = HashSet::new();
                let mut keys: Vec<String> = Vec::new();
                let mut cursor = "0".to_owned();
                loop {
                    let r = c.call(&["SCAN", &cursor, "MATCH", &pattern, "COUNT", "1000"]).await.map_err(e)?;
                    let parts = r.items();
                    cursor = parts.first().and_then(Resp::text).unwrap_or_else(|| "0".into());
                    for k in parts.get(1).map(Resp::items).unwrap_or_default() {
                        if let Some(k) = k.text()
                            && seen.insert(k.clone())
                        {
                            keys.push(k);
                        }
                    }
                    if keys.len() > MAX_KEYS {
                        return Err(kit::err(&ctx.node.name, format!("The pattern matches more than {MAX_KEYS} keys; narrow it")));
                    }
                    if cursor == "0" {
                        break;
                    }
                }
                if !kit::bool_param(ctx, &self.desc, "getValues", i)? {
                    return Ok(out_item(&pattern, json!(keys)));
                }
                let mut obj = Map::new();
                for k in keys {
                    let ty = c.call(&["TYPE", &k]).await.map_err(e)?.text().unwrap_or_default();
                    let v = match ty.as_str() {
                        "string" => c.call(&["GET", &k]).await.map_err(e)?.text().map_or(Value::Null, |t| parse_value(&t)),
                        "hash" => {
                            let r = c.call(&["HGETALL", &k]).await.map_err(e)?;
                            Value::Object(r.items().chunks(2).filter(|p| p.len() == 2).map(|p| (p[0].text().unwrap_or_default(), bulk_value(&p[1]))).collect())
                        }
                        "list" => Value::Array(c.call(&["LRANGE", &k, "0", "-1"]).await.map_err(e)?.items().iter().map(bulk_value).collect()),
                        "set" => Value::Array(c.call(&["SMEMBERS", &k]).await.map_err(e)?.items().iter().map(bulk_value).collect()),
                        _ => Value::Null,
                    };
                    obj.insert(k, v);
                }
                Ok(vec![Item::new(obj)])
            }
            "llen" => {
                let list = self.required(ctx, "list", "List", i)?;
                let n = c.call(&["LLEN", &list]).await.map_err(e)?.int();
                Ok(out_item(&list, json!(n)))
            }
            "pop" => {
                let list = self.required(ctx, "list", "List", i)?;
                let prop = self.s(ctx, "propertyName", i)?;
                let prop = if prop.is_empty() { "propertyName".to_owned() } else { prop };
                let cmd = if kit::bool_param(ctx, &self.desc, "tail", i)? { "RPOP" } else { "LPOP" };
                let r = c.call(&[cmd, &list]).await.map_err(e)?;
                Ok(out_item(&prop, r.text().map_or(Value::Null, |t| parse_value(&t))))
            }
            "publish" => {
                let channel = self.required(ctx, "channel", "Channel", i)?;
                let data = self.s(ctx, "messageData", i)?;
                let n = c.call(&["PUBLISH", &channel, &data]).await.map_err(e)?.int();
                Ok(vec![Item::from_json(json!({ "channel": channel, "receivers": n }))])
            }
            "push" => {
                let list = self.required(ctx, "list", "List", i)?;
                let data = self.s(ctx, "messageData", i)?;
                let cmd = if kit::bool_param(ctx, &self.desc, "tail", i)? { "RPUSH" } else { "LPUSH" };
                c.call(&[cmd, &list, &data]).await.map_err(e)?;
                Ok(vec![input])
            }
            "set" => {
                let key = self.required(ctx, "key", "Key", i)?;
                let raw = kit::param(ctx, &self.desc, "value", i)?.unwrap_or(Value::Null);
                // non-string values (expression results) are stored as JSON text
                let text = match &raw {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                match self.s(ctx, "keyType", i)?.as_str() {
                    "hash" => {
                        let Value::Object(m) = parse_value(&text) else {
                            return Err(node_err(ctx, "A hash needs a JSON object as value"));
                        };
                        let mut args: Vec<String> = vec!["HSET".into(), key.clone()];
                        for (f, v) in m {
                            args.push(f);
                            args.push(display(&v));
                        }
                        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                        if refs.len() > 2 {
                            c.call(&refs).await.map_err(e)?;
                        }
                    }
                    ty @ ("list" | "sets") => {
                        let Value::Array(a) = parse_value(&text) else {
                            return Err(node_err(ctx, "A list or set needs a JSON array as value"));
                        };
                        let mut args: Vec<String> = vec![(if ty == "list" { "RPUSH" } else { "SADD" }).into(), key.clone()];
                        args.extend(a.iter().map(display));
                        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                        if refs.len() > 2 {
                            c.call(&refs).await.map_err(e)?;
                        }
                    }
                    _ => {
                        c.call(&["SET", &key, &text]).await.map_err(e)?;
                    }
                }
                self.expire_if(ctx, c, &key, i).await?;
                Ok(vec![input])
            }
            other => Err(node_err(ctx, format!("Unsupported operation `{other}`"))),
        }
    }
}

#[async_trait]
impl FlowNode for RedisNode {
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
        let get = |k: &str| kit::cred_str(&cred, k);
        let host = if get("host").is_empty() { "localhost".to_owned() } else { get("host") };
        let port = get("port").parse().unwrap_or(6379);
        let ssl = matches!(cred.get("ssl"), Some(Value::Bool(true)));
        let fail = |m: String| kit::err(&name, format!("Redis: {m}"));
        let mut conn = Conn::open(&host, port, ssl).await.map_err(fail)?;
        let (user, pass) = (get("user"), get("password"));
        if !pass.is_empty() {
            let r = if user.is_empty() { conn.call(&["AUTH", &pass]).await } else { conn.call(&["AUTH", &user, &pass]).await };
            r.map_err(|m| kit::err(&name, format!("Redis authentication failed: {m}")))?;
        }
        let db = get("database");
        if !db.is_empty() && db != "0" {
            conn.call(&["SELECT", &db]).await.map_err(fail)?;
        }
        let mut out = Vec::new();
        for i in 0..input_len {
            let res = self.run_item(ctx, &mut conn, i).await;
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-redis`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(RedisNode { desc: description(), creds: creds.clone() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_parses_numbers_and_skips_sections() {
        let m = parse_info("# Server\r\nredis_version:7.2.4\r\nuptime_in_seconds:42\r\nused_memory_rss_human:1.5M\r\nload:0.5\r\n\r\ndb0:keys=1,expires=0\r\n");
        assert_eq!(m["redis_version"], "7.2.4");
        assert_eq!(m["uptime_in_seconds"], 42);
        assert_eq!(m["load"], 0.5);
        assert_eq!(m["db0"], "keys=1,expires=0");
    }

    #[test]
    fn values_parse_as_json_when_possible() {
        assert_eq!(parse_value("{\"a\":1}"), json!({"a": 1}));
        assert_eq!(parse_value("12"), json!(12));
        assert_eq!(parse_value("hello"), json!("hello"));
    }
}
