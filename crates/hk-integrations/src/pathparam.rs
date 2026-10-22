//! Helpers shared by REST integrations whose route URLs carry `{param}` placeholders
//! (GitHub, Gitea, n8n): placeholder expansion with percent-encoding, resource-locator
//! unwrapping, tolerant list parsing, and a "look up the file sha first" node wrapper.
//!
//! Placeholders are plain `{name}` rather than `{{ $parameter.name }}` expressions: the
//! values are looked up directly in the node parameters, so no JS engine round trip per
//! request and no way for a parameter value to inject expression syntax into a URL.

use std::fmt::Write as _;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, NodeContext, NodeOutput, PairedItem};

use crate::rest::{RestNode, RestRequest};
use crate::template::display;

pub fn node_err(ctx: &NodeContext, message: impl Into<String>) -> FlowError {
    FlowError::Node { node: ctx.node.name.clone(), message: message.into() }
}

/// n8n resource locators (`{__rl: true, value, mode}`) collapse to their value.
pub fn unwrap_rl(v: Value) -> Value {
    match v {
        Value::Object(mut m) if m.contains_key("__rl") => m.remove("value").unwrap_or(Value::Null),
        other => other,
    }
}

/// Non-empty string form of a parameter at dotted `path`; `defaults` fills in what n8n
/// exports omit.
pub fn opt_str(
    ctx: &NodeContext,
    defaults: &Map<String, Value>,
    path: &str,
    i: usize,
) -> Result<Option<String>, FlowError> {
    let v = match ctx.eval_param_at(path, i)? {
        Some(v) => Some(v),
        None => defaults.get(path).cloned(),
    };
    Ok(v.map(unwrap_rl).map(|v| display(&v)).filter(|s| !s.is_empty()))
}

pub fn req_str(
    ctx: &NodeContext,
    defaults: &Map<String, Value>,
    path: &str,
    i: usize,
) -> Result<String, FlowError> {
    opt_str(ctx, defaults, path, i)?
        .ok_or_else(|| node_err(ctx, format!("Parameter `{path}` is required")))
}

/// Top-level property defaults of a description (first declaration wins, like `RestNode`).
pub fn defaults_of(desc: &NodeTypeDescription) -> Map<String, Value> {
    let mut m = Map::new();
    for p in &desc.properties {
        if let Some(d) = &p.default {
            m.entry(p.name.clone()).or_insert_with(|| d.clone());
        }
    }
    m
}

/// Percent-encode everything but RFC 3986 unreserved characters.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Replace `{name}` in the request URL with the encoded parameter value. `{+name}` keeps
/// `/` (file paths). The scheme and host part never contain braces.
pub fn expand(
    req: &mut RestRequest,
    ctx: &NodeContext,
    defaults: &Map<String, Value>,
    i: usize,
) -> Result<(), FlowError> {
    let mut out = String::with_capacity(req.url.len());
    let mut rest = req.url.as_str();
    while let Some(s) = rest.find('{') {
        out.push_str(&rest[..s]);
        let after = &rest[s + 1..];
        let Some(e) = after.find('}') else {
            return Err(node_err(ctx, "Unterminated `{` in route URL"));
        };
        let spec = &after[..e];
        let (keep_slash, name) = match spec.strip_prefix('+') {
            Some(n) => (true, n),
            None => (false, spec),
        };
        let value = req_str(ctx, defaults, name, i)?;
        if keep_slash {
            let enc: Vec<String> = value.trim_matches('/').split('/').map(encode).collect();
            out.push_str(&enc.join("/"));
        } else {
            out.push_str(&encode(&value));
        }
        rest = &after[e + 1..];
    }
    out.push_str(rest);
    req.url = out;
    Ok(())
}

/// Strings out of the shapes a list parameter takes: `"a, b"`, `["a"]`, n8n fixedCollection
/// `[{"label": "a"}]` or `{"label": [{"label": "a"}]}`.
pub fn string_list(v: &Value) -> Vec<String> {
    fn walk(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.extend(s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned)),
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(m) => m.values().for_each(|x| walk(x, out)),
            Value::Number(n) => out.push(n.to_string()),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, &mut out);
    out
}

/// `{"branch": {"branch": "x"}}` and `{"branch": "x"}` both give `x`.
pub fn nested_str(v: &Value, outer: &str, inner: &str) -> Option<String> {
    let o = v.get(outer)?;
    let s = match o {
        Value::Object(m) => m.get(inner).map(display),
        other => Some(display(other)),
    }?;
    (!s.is_empty()).then_some(s)
}

/// Wraps a [`RestNode`] for forges whose file edit/delete need the current blob sha: for
/// `resource=file` with an operation in `needs_sha`, each item first runs the node's `get`
/// operation, then the real one with the sha injected as hidden parameter `__sha`. Any
/// other call goes straight to the inner node.
pub struct ShaFirst {
    inner: RestNode,
    defaults: Map<String, Value>,
    needs_sha: &'static [&'static str],
}

impl std::fmt::Debug for ShaFirst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShaFirst").field("inner", &self.inner).finish_non_exhaustive()
    }
}

impl ShaFirst {
    pub fn new(inner: RestNode, needs_sha: &'static [&'static str]) -> Self {
        let defaults = defaults_of(inner.description());
        Self { inner, defaults, needs_sha }
    }

    fn item_ctx(ctx: &NodeContext, i: usize, operation: &str, sha: Option<&str>) -> NodeContext {
        let mut c = ctx.clone();
        c.input = vec![vec![ctx.input[0][i].clone()]];
        c.node.parameters.insert("operation".into(), Value::String(operation.to_owned()));
        if let Some(sha) = sha {
            c.node.parameters.insert("__sha".into(), Value::String(sha.to_owned()));
        }
        c
    }
}

#[async_trait]
impl FlowNode for ShaFirst {
    fn description(&self) -> &NodeTypeDescription {
        self.inner.description()
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let resource = opt_str(ctx, &self.defaults, "resource", 0)?.unwrap_or_default();
        let op = opt_str(ctx, &self.defaults, "operation", 0)?.unwrap_or_default();
        let items = ctx.input.first().map_or(0, Vec::len);
        if resource != "file" || !self.needs_sha.contains(&op.as_str()) || items == 0 {
            return self.inner.execute(ctx).await;
        }
        let mut out = Vec::new();
        for i in 0..items {
            // the lookup reads the file from the branch the write targets
            let mut lookup = Self::item_ctx(ctx, i, "get", None);
            if let Some(b) = ctx
                .eval_param_at("additionalParameters", i)?
                .and_then(|v| nested_str(&v, "branch", "branch"))
            {
                lookup.node.parameters.insert("__ref".into(), Value::String(b));
            }
            let got = self.inner.execute(&lookup).await?;
            let first = got.into_iter().next().and_then(|v| v.into_iter().next());
            let Some(first) = first else {
                return Err(node_err(ctx, "File lookup returned nothing"));
            };
            if first.error.is_some() {
                out.push(retarget(first, i));
                continue;
            }
            let Some(sha) = first.json.get("sha").and_then(Value::as_str) else {
                return Err(node_err(ctx, "Could not determine the sha of the file (is the path a directory?)"));
            };
            let run = Self::item_ctx(ctx, i, &op, Some(sha));
            let res = self.inner.execute(&run).await?;
            out.extend(res.into_iter().next().unwrap_or_default().into_iter().map(|o| retarget(o, i)));
        }
        Ok(vec![out])
    }
}

fn retarget(mut item: hk_flow::Item, i: usize) -> hk_flow::Item {
    item.paired_item = Some(PairedItem::Index(i));
    item
}

pub fn put(body: &mut Map<String, Value>, k: &str, v: Option<Value>) {
    if let Some(v) = v {
        body.insert(k.to_owned(), v);
    }
}

/// Collection members the user added, copied into the body as typed.
pub fn collect_into(
    ctx: &NodeContext,
    i: usize,
    coll: &str,
    keys: &[&str],
    body: &mut Map<String, Value>,
) -> Result<(), FlowError> {
    for k in keys {
        match ctx.eval_param_at(&format!("{coll}.{k}"), i)? {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) if s.is_empty() => {}
            Some(v) if matches!(*k, "labels" | "assignees") => {
                body.insert((*k).to_owned(), json!(string_list(&v)));
            }
            Some(v) => {
                body.insert((*k).to_owned(), v);
            }
        }
    }
    Ok(())
}

pub fn query_from(
    ctx: &NodeContext,
    i: usize,
    coll: &str,
    keys: &[&str],
    req: &mut RestRequest,
) -> Result<(), FlowError> {
    for k in keys {
        if let Some(v) = ctx.eval_param_at(&format!("{coll}.{k}"), i)?.filter(|v| !v.is_null()) {
            let s = display(&v);
            if !s.is_empty() {
                req.query.push(((*k).to_owned(), s));
            }
        }
    }
    Ok(())
}

/// `author` / `committer` objects of the contents API.
pub fn person(extra: &Value, who: &str) -> Option<Value> {
    let name = extra.get(format!("{who}Name")).and_then(Value::as_str).map(str::to_owned).or_else(|| nested_str(extra, who, "name"))?;
    let email = extra.get(format!("{who}Email")).and_then(Value::as_str).map(str::to_owned).or_else(|| nested_str(extra, who, "email"))?;
    Some(json!({"name": name, "email": email}))
}

