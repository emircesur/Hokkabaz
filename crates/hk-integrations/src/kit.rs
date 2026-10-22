//! Plumbing shared by the hand-written integration nodes (Telegram, Matrix, Gotify, email).
//!
//! `RestNode` covers declarative nodes; these helpers cover the rest: nodes that pick the
//! token per operation, upload multipart files, or speak SMTP/IMAP. Everything here keeps
//! credential values out of errors: HTTP failures are reported without the URL (it may carry
//! a bot token) and response bodies are only quoted for their human message field.

// Which helpers are used depends on the enabled `int-*` features.
#![allow(dead_code)]

use std::time::Duration;

use base64::Engine;
use serde_json::{Map, Value, json};

use hk_flow::description::{NodeProperty, NodeTypeDescription, PropertyType};
use hk_flow::{ErrorMode, FlowError, Item, NodeContext, PairedItem};

use crate::credentials::CredentialTypes;
use crate::describe::{Choice, Prop};
use crate::template::display;

pub(crate) fn err(node: &str, message: impl Into<String>) -> FlowError {
    FlowError::Node {
        node: node.to_owned(),
        message: message.into(),
    }
}

pub(crate) fn node_err(ctx: &NodeContext, message: impl Into<String>) -> FlowError {
    err(&ctx.node.name, message)
}

/// Parameter value for item `i`, falling back to the description default (n8n exports omit
/// defaults).
pub(crate) fn param(
    ctx: &NodeContext,
    desc: &NodeTypeDescription,
    name: &str,
    i: usize,
) -> Result<Option<Value>, FlowError> {
    if let Some(v) = ctx.eval_param_at(name, i)? {
        return Ok(Some(v));
    }
    Ok(desc.property(name).and_then(|p| p.default.clone()))
}

/// String parameter; empty when unset.
pub(crate) fn str_param(
    ctx: &NodeContext,
    desc: &NodeTypeDescription,
    name: &str,
    i: usize,
) -> Result<String, FlowError> {
    Ok(param(ctx, desc, name, i)?
        .map(|v| display(&v))
        .unwrap_or_default())
}

pub(crate) fn bool_param(
    ctx: &NodeContext,
    desc: &NodeTypeDescription,
    name: &str,
    i: usize,
) -> Result<bool, FlowError> {
    Ok(matches!(
        param(ctx, desc, name, i)?,
        Some(Value::Bool(true))
    ))
}

/// `additionalFields.<key>`-style member; `None` when the user did not add it.
pub(crate) fn field(ctx: &NodeContext, path: &str, i: usize) -> Result<Option<Value>, FlowError> {
    Ok(ctx.eval_param_at(path, i)?.filter(|v| !v.is_null()))
}

pub(crate) fn field_str(
    ctx: &NodeContext,
    path: &str,
    i: usize,
) -> Result<Option<String>, FlowError> {
    Ok(field(ctx, path, i)?
        .map(|v| display(&v))
        .filter(|s| !s.is_empty()))
}

/// Decrypted credential fields with type defaults applied. Use and drop.
pub(crate) async fn credential(
    ctx: &NodeContext,
    creds: &CredentialTypes,
    name: &str,
) -> Result<Map<String, Value>, FlowError> {
    let reference = ctx
        .node
        .credentials
        .as_ref()
        .and_then(|m| m.get(name))
        .ok_or_else(|| node_err(ctx, format!("No credentials of type `{name}` selected")))?;
    let resolver = ctx
        .hooks
        .credentials
        .as_ref()
        .ok_or_else(|| node_err(ctx, "Credentials are not available in this runtime"))?;
    let raw = resolver.resolve(name, reference).await?;
    Ok(creds.with_defaults(name, &raw))
}

pub(crate) fn cred_str(data: &Map<String, Value>, key: &str) -> String {
    data.get(key).map(display).unwrap_or_default()
}

pub(crate) fn client(node: &str, timeout: Duration) -> Result<reqwest::Client, FlowError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| {
            err(
                node,
                format!("Could not build HTTP client: {}", e.without_url()),
            )
        })
}

fn dotted<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, seg| match cur {
        Value::Object(m) => m.get(seg),
        Value::Array(a) => a.get(seg.parse::<usize>().ok()?),
        _ => None,
    })
}

/// `base` + `segments`, each percent-encoded as one path segment (ids may hold `/`, `:`).
pub(crate) fn join_url(
    node: &str,
    base: &str,
    segments: &[String],
) -> Result<reqwest::Url, FlowError> {
    let mut u = reqwest::Url::parse(base.trim())
        .map_err(|_| err(node, "The server URL is not a valid URL"))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(err(node, "The server URL must be http or https"));
    }
    u.path_segments_mut()
        .map_err(|()| err(node, "The server URL cannot be a base"))?
        .pop_if_empty()
        .extend(segments);
    Ok(u)
}

/// JSON body without reqwest's `json` feature (the crate keeps its reqwest feature set minimal).
pub(crate) fn json_body(rb: reqwest::RequestBuilder, v: &Value) -> reqwest::RequestBuilder {
    rb.header("content-type", "application/json")
        .body(v.to_string())
}

/// Send `rb`, parse the JSON (or `{data: text}`) body, map failures to node errors.
/// `error_paths` are tried in order to find a human message in an error body.
pub(crate) async fn call(
    node: &str,
    rb: reqwest::RequestBuilder,
    error_paths: &[&str],
) -> Result<Value, FlowError> {
    let res = rb
        .send()
        .await
        .map_err(|e| err(node, format!("The request failed: {}", e.without_url())))?;
    let status = res.status();
    let text = res.text().await.map_err(|e| {
        err(
            node,
            format!("Could not read the response: {}", e.without_url()),
        )
    })?;
    let parsed = if text.trim().is_empty() {
        None
    } else {
        Some(serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({ "data": text })))
    };
    if !status.is_success() {
        let detail = parsed.as_ref().and_then(|b| {
            error_paths.iter().find_map(|p| match dotted(b, p) {
                Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
                Some(v @ (Value::Number(_) | Value::Bool(_))) => Some(v.to_string()),
                _ => None,
            })
        });
        let mut msg = format!(
            "The service returned an error: {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        );
        if let Some(d) = detail {
            msg.push_str(": ");
            msg.extend(d.chars().take(300));
        }
        if matches!(status.as_u16(), 401 | 403) {
            msg.push_str(" (check the credentials)");
        }
        return Err(err(node, msg));
    }
    Ok(parsed.unwrap_or_else(|| json!({ "success": true })))
}

/// Object body as an item; other JSON shapes are wrapped as `{ value }`.
pub(crate) fn to_item(v: Value) -> Item {
    match v {
        Value::Object(m) => Item::new(m),
        other => Item::from_json(json!({ "value": other })),
    }
}

/// Fold one input item's outcome into `out`: pair outputs with the input, or turn a node
/// error into an error item when the node continues on fail.
pub(crate) fn settle(
    ctx: &NodeContext,
    i: usize,
    res: Result<Vec<Item>, FlowError>,
    out: &mut Vec<Item>,
) -> Result<(), FlowError> {
    let carry = matches!(
        ctx.node.error_mode(),
        ErrorMode::ContinueRegularOutput | ErrorMode::ContinueErrorOutput
    );
    match res {
        Ok(items) => out.extend(items.into_iter().map(|mut o| {
            o.paired_item = Some(PairedItem::Index(i));
            o
        })),
        Err(FlowError::Node { message, .. }) if carry => {
            let mut o = Item::from_json(json!({ "error": message }));
            o.error = Some(json!({ "message": message }));
            o.paired_item = Some(PairedItem::Index(i));
            out.push(o);
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Decoded binary property of an input item.
#[derive(Debug, Clone)]
pub struct BinFile {
    pub data: Vec<u8>,
    pub mime: String,
    pub name: String,
}

pub(crate) fn binary_file(
    ctx: &NodeContext,
    item: &Item,
    prop: &str,
) -> Result<BinFile, FlowError> {
    let bin = item
        .binary
        .as_ref()
        .and_then(|b| b.get(prop))
        .ok_or_else(|| node_err(ctx, format!("Item has no binary property `{prop}`")))?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(bin.data.as_bytes())
        .map_err(|_| node_err(ctx, format!("Binary property `{prop}` is not valid base64")))?;
    Ok(BinFile {
        data,
        mime: if bin.mime_type.is_empty() {
            "application/octet-stream".into()
        } else {
            bin.mime_type.clone()
        },
        name: bin.file_name.clone().unwrap_or_else(|| prop.to_owned()),
    })
}

/// `multipart/form-data` body, built by hand: no extra reqwest feature needed.
#[derive(Debug)]
pub(crate) struct Multipart {
    boundary: String,
    body: Vec<u8>,
}

impl Multipart {
    pub fn new() -> Self {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        Self {
            boundary: format!("hkboundary{n:x}"),
            body: Vec::new(),
        }
    }

    fn quoted(s: &str) -> String {
        s.chars()
            .filter(|c| !matches!(c, '"' | '\r' | '\n'))
            .collect()
    }

    pub fn text(&mut self, name: &str, value: &str) {
        let head = format!(
            "--{}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n",
            self.boundary,
            Self::quoted(name)
        );
        self.body.extend_from_slice(head.as_bytes());
        self.body.extend_from_slice(value.as_bytes());
        self.body.extend_from_slice(b"\r\n");
    }

    pub fn file(&mut self, name: &str, file: &BinFile) {
        let head = format!(
            "--{}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
            self.boundary,
            Self::quoted(name),
            Self::quoted(&file.name),
            Self::quoted(&file.mime)
        );
        self.body.extend_from_slice(head.as_bytes());
        self.body.extend_from_slice(&file.data);
        self.body.extend_from_slice(b"\r\n");
    }

    /// `(content-type, body)`.
    pub fn finish(mut self) -> (String, Vec<u8>) {
        self.body
            .extend_from_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        (
            format!("multipart/form-data; boundary={}", self.boundary),
            self.body,
        )
    }
}

/// `multiOptions` property (the `Prop` builder has no constructor for it).
pub(crate) fn multi_options(
    name: &str,
    display: &str,
    choices: &[(&str, &str)],
    default: Value,
) -> NodeProperty {
    let mut p = Prop::options(
        name,
        display,
        choices.iter().map(|(n, v)| Choice::new(n, *v)).collect(),
    )
    .build();
    p.prop_type = PropertyType::MultiOptions;
    p.default = Some(default);
    p
}

/// Insert `prop` before the property called `before` (appends when absent).
pub(crate) fn insert_before(desc: &mut NodeTypeDescription, before: &str, prop: NodeProperty) {
    let at = desc
        .properties
        .iter()
        .position(|p| p.name == before)
        .unwrap_or(desc.properties.len());
    desc.properties.insert(at, prop);
}

/// Versions a node serves; n8n resolves nodes by exact `typeVersion`.
pub(crate) fn serve_versions(desc: &mut NodeTypeDescription, versions: &[f64]) {
    use hk_flow::version::{NodeVersions, Version};
    let vs: Vec<Version> = versions.iter().filter_map(|v| Version::new(*v)).collect();
    desc.default_version = vs.iter().copied().max();
    desc.version = NodeVersions::Many(vs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_shape() {
        let mut m = Multipart::new();
        m.text("chat_id", "42");
        m.file(
            "photo",
            &BinFile {
                data: b"\x89PNG".to_vec(),
                mime: "image/png".into(),
                name: "a\"b.png".into(),
            },
        );
        let (ct, body) = m.finish();
        let b = ct.strip_prefix("multipart/form-data; boundary=").unwrap();
        let s = String::from_utf8_lossy(&body);
        assert!(s.starts_with(&format!("--{b}\r\n")));
        assert!(s.contains("name=\"chat_id\"\r\n\r\n42\r\n"));
        assert!(s.contains("filename=\"ab.png\"\r\nContent-Type: image/png"));
        assert!(s.ends_with(&format!("--{b}--\r\n")));
    }
}
