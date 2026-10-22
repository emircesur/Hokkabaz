//! `RestNode`: n8n "declarative-style" node over HTTP.
//!
//! The node description carries the routing ([`Routing`]): each `operation` choice names the
//! request it makes, each parameter says where it goes (body/query/header). `RestNode`
//! interprets that per input item, applies the credential, pages list operations, maps
//! errors and turns the response into items. A typical integration is a [`NodeBuilder`]
//! description plus a [`RestSpec`] (see `ntfy.rs`).
//!
//! Not covered (use a hand-written `FlowNode`): multipart bodies, binary responses,
//! OAuth token refresh.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use hk_flow::description::{NodeProperty, NodeTypeDescription, PropertyType};
use hk_flow::{
    CredentialResolver, ErrorMode, FlowError, FlowNode, Item, NodeContext, NodeOutput, PairedItem,
};

use crate::credentials::{AppliedAuth, CredentialTypes};
use crate::describe::{Pagination, RouteOutput, RouteSend, Routing, SendKind, is_visible};
use crate::template::{self, display};

/// Marker key: the planned body is this value, not an object of fields.
const RAW_BODY: &str = "$raw";
const RESOURCE: &str = "resource";
const OPERATION: &str = "operation";

/// Request under construction. `pre_send` hooks may rewrite any part of it.
#[derive(Debug, Clone)]
pub struct RestRequest {
    pub method: String,
    /// Absolute URL.
    pub url: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: RestBody,
}

#[derive(Debug, Clone)]
pub enum RestBody {
    None,
    Json(Value),
    Form(Vec<(String, String)>),
    Raw { content_type: String, data: Vec<u8> },
}

/// Escape hatch run after routing and before auth: `(request, ctx, item_index)`.
pub type PreSend =
    Arc<dyn Fn(&mut RestRequest, &NodeContext, usize) -> Result<(), FlowError> + Send + Sync>;

/// Credential a node uses.
#[derive(Debug, Clone)]
pub struct CredentialUse {
    pub name: String,
    pub required: bool,
}

pub struct RestSpec {
    /// Base URL template; may use `$credentials.*` (`={{$credentials.serverUrl}}`).
    pub base_url: String,
    pub credential: Option<CredentialUse>,
    pub default_headers: Vec<(String, String)>,
    pub timeout: Duration,
    /// Dotted paths tried, in order, to find a human message in an error body.
    pub error_paths: Vec<String>,
    pub pre_send: Option<PreSend>,
    /// Safety cap for paginated operations.
    pub max_pages: usize,
}

impl fmt::Debug for RestSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RestSpec")
            .field("base_url", &self.base_url)
            .field("credential", &self.credential)
            .finish_non_exhaustive()
    }
}

impl RestSpec {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.to_owned(),
            credential: None,
            default_headers: Vec::new(),
            timeout: Duration::from_secs(30),
            error_paths: [
                "message",
                "error.message",
                "error",
                "errors.0.message",
                "detail",
                "error_description",
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
            pre_send: None,
            max_pages: 1000,
        }
    }

    pub fn credential(mut self, name: &str, required: bool) -> Self {
        self.credential = Some(CredentialUse {
            name: name.to_owned(),
            required,
        });
        self
    }

    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.default_headers.push((k.to_owned(), v.to_owned()));
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    pub fn error_path(mut self, p: &str) -> Self {
        self.error_paths.insert(0, p.to_owned());
        self
    }

    pub fn pre_send(
        mut self,
        f: impl Fn(&mut RestRequest, &NodeContext, usize) -> Result<(), FlowError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.pre_send = Some(Arc::new(f));
        self
    }
}

pub struct RestNode {
    desc: NodeTypeDescription,
    spec: RestSpec,
    creds: CredentialTypes,
}

impl fmt::Debug for RestNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RestNode")
            .field("type", &self.desc.name)
            .finish_non_exhaustive()
    }
}

impl RestNode {
    pub fn new(desc: NodeTypeDescription, spec: RestSpec, creds: &CredentialTypes) -> Self {
        Self {
            desc,
            spec,
            creds: creds.clone(),
        }
    }
}

// ------------------------------------------------------------------ helpers

fn node_err(ctx: &NodeContext, message: impl Into<String>) -> FlowError {
    FlowError::Node {
        node: ctx.node.name.clone(),
        message: message.into(),
    }
}

/// Evaluate a routing value: `=`-prefixed or `{{ }}`-containing strings are expressions.
fn ev(ctx: &NodeContext, v: &Value, i: usize) -> Result<Value, FlowError> {
    match v {
        Value::String(s) if !s.starts_with('=') && s.contains("{{") => {
            ctx.eval_param(&Value::String(format!("={s}")), i)
        }
        other => ctx.eval_param(other, i),
    }
}

/// Parameter value, falling back to the description default (n8n exports omit defaults).
fn param(
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

fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    // keys that contain dots themselves (`@odata.nextLink`) match whole first
    if let Some(x) = v.as_object().and_then(|m| m.get(path)) {
        return Some(x);
    }
    let mut cur = v;
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        cur = match cur {
            Value::Object(m) => m.get(seg)?,
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn set_path(obj: &mut Map<String, Value>, path: &str, v: Value) {
    let mut parts = path.split('.').peekable();
    let mut cur = obj;
    while let Some(p) = parts.next() {
        if parts.peek().is_none() {
            cur.insert(p.to_owned(), v);
            return;
        }
        let slot = cur
            .entry(p.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if !slot.is_object() {
            *slot = Value::Object(Map::new());
        }
        match slot {
            Value::Object(m) => cur = m,
            _ => return,
        }
    }
}

/// Keep only `fields` (dotted paths) of `v`.
fn pick(v: &Value, fields: &[String]) -> Value {
    let mut out = Map::new();
    for f in fields {
        if let Some(x) = get_path(v, f) {
            set_path(&mut out, f, x.clone());
        }
    }
    Value::Object(out)
}

fn form_enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `url` with `query` appended, query values percent-encoded (what the signer canonicalises).
fn rb_url(url: &str, query: &[&(String, String)]) -> Result<reqwest::Url, String> {
    let mut u = reqwest::Url::parse(url).map_err(|_| "Invalid URL".to_owned())?;
    if !query.is_empty() {
        let q = query
            .iter()
            .map(|(k, v)| format!("{}={}", form_enc(k), form_enc(v)))
            .collect::<Vec<_>>()
            .join("&");
        u.set_query(Some(&q));
    }
    Ok(u)
}

/// Minimal XML -> JSON for services that answer in XML (AWS Query APIs): element name keys,
/// repeated elements become arrays, leaf text becomes a string. Attributes are ignored.
fn xml_to_json(xml: &str) -> Option<Value> {
    fn parse(s: &str, pos: &mut usize) -> Option<(String, Value)> {
        let b = s.as_bytes();
        while *pos < b.len() && b[*pos] != b'<' {
            *pos += 1;
        }
        // skip declarations / comments
        while s[*pos..].starts_with("<?") || s[*pos..].starts_with("<!") {
            *pos += s[*pos..].find('>')? + 1;
            while *pos < b.len() && b[*pos] != b'<' {
                *pos += 1;
            }
        }
        if *pos >= b.len() || b[*pos] != b'<' {
            return None;
        }
        let end = *pos + s[*pos..].find('>')?;
        let head = &s[*pos + 1..end];
        let name = head
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()?
            .rsplit(':')
            .next()?
            .to_owned();
        let self_closing = head.ends_with('/');
        *pos = end + 1;
        if self_closing {
            return Some((name, Value::String(String::new())));
        }
        let mut children: Vec<(String, Value)> = Vec::new();
        let mut text = String::new();
        loop {
            let next = s[*pos..].find('<')? + *pos;
            text.push_str(&s[*pos..next]);
            *pos = next;
            if s[*pos..].starts_with("</") {
                *pos += s[*pos..].find('>')? + 1;
                break;
            }
            if s[*pos..].starts_with("<![CDATA[") {
                let e = s[*pos..].find("]]>")? + *pos;
                text.push_str(&s[*pos + 9..e]);
                *pos = e + 3;
                continue;
            }
            children.push(parse(s, pos)?);
        }
        if children.is_empty() {
            let t = text
                .trim()
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&");
            return Some((name, Value::String(t)));
        }
        let mut obj = Map::new();
        for (k, v) in children {
            match obj.get_mut(&k) {
                Some(Value::Array(a)) => a.push(v),
                Some(old) => {
                    let first = old.take();
                    *old = Value::Array(vec![first, v]);
                }
                None => {
                    obj.insert(k, v);
                }
            }
        }
        Some((name, Value::Object(obj)))
    }
    let mut pos = 0;
    let (name, v) = parse(xml, &mut pos)?;
    let mut root = Map::new();
    root.insert(name, v);
    Some(Value::Object(root))
}

/// First row = column names; later rows become objects (`row_number` is the sheet row, 2-based).
fn table_rows(table: &[Value]) -> Vec<Value> {
    let Some(Value::Array(head)) = table.first() else {
        return Vec::new();
    };
    let names: Vec<String> = head.iter().map(display).collect();
    table
        .iter()
        .enumerate()
        .skip(1)
        .map(|(n, row)| {
            let cells = row.as_array().map(Vec::as_slice).unwrap_or(&[]);
            let mut m = Map::new();
            m.insert("row_number".into(), json!(n + 1));
            for (k, name) in names.iter().enumerate() {
                m.insert(name.clone(), cells.get(k).cloned().unwrap_or(json!("")));
            }
            Value::Object(m)
        })
        .collect()
}

/// Set a paging field where the API wants it: query string, or the JSON body.
fn set_page(req: &mut RestRequest, in_body: bool, k: &str, v: Value) {
    if in_body {
        let mut obj = match std::mem::replace(&mut req.body, RestBody::None) {
            RestBody::Json(Value::Object(o)) => o,
            _ => Map::new(),
        };
        obj.insert(k.to_owned(), v);
        req.body = RestBody::Json(Value::Object(obj));
    } else {
        req.query.retain(|(n, _)| n != k);
        req.query.push((k.to_owned(), display(&v)));
    }
}

fn empty(v: &Value) -> bool {
    matches!(v, Value::Null) || matches!(v, Value::String(s) if s.is_empty())
}

// -------------------------------------------------------------------- plan

struct Plan {
    req: RestRequest,
    route: Routing,
    simplify: bool,
    return_all: bool,
    limit: Option<usize>,
}

struct Resolved {
    data: Map<String, Value>,
    auth: AppliedAuth,
}

impl RestNode {
    fn lookup<'a>(&'a self, ctx: &'a NodeContext, i: usize) -> impl Fn(&str) -> Option<Value> + 'a {
        move |k| param(ctx, &self.desc, k, i).ok().flatten()
    }

    /// The `routing` of the chosen operation option.
    fn operation_route(&self, ctx: &NodeContext, i: usize) -> Result<Routing, FlowError> {
        let look = self.lookup(ctx, i);
        let op = look(OPERATION);
        let wanted = op
            .as_ref()
            .map(display)
            .ok_or_else(|| node_err(ctx, "Parameter 'Operation' is not set"))?;
        for p in self.desc.properties.iter().filter(|p| p.name == OPERATION) {
            if !is_visible(p.display_options.as_ref(), &look) {
                continue;
            }
            for o in p.options.iter().flatten() {
                if o.get("value").map(display).as_deref() == Some(wanted.as_str()) {
                    return o
                        .get("routing")
                        .and_then(|r| serde_json::from_value::<Routing>(r.clone()).ok())
                        .filter(|r| r.request.is_some())
                        .ok_or_else(|| {
                            node_err(ctx, format!("Operation `{wanted}` has no request routing"))
                        });
                }
            }
        }
        Err(node_err(ctx, format!("Unknown operation `{wanted}`")))
    }

    fn plan(&self, ctx: &NodeContext, i: usize, creds: &Resolved) -> Result<Plan, FlowError> {
        let route = self.operation_route(ctx, i)?;
        let rq = route.request.clone().unwrap_or_default();
        let look = self.lookup(ctx, i);

        let url_t = match rq.url.as_deref() {
            Some(u) => display(&ev(ctx, &Value::String(u.to_owned()), i)?),
            None => String::new(),
        };
        let url = if url_t.starts_with("http://") || url_t.starts_with("https://") {
            url_t
        } else {
            let base = template::render(&self.spec.base_url, &creds.data)
                .map_err(|e| node_err(ctx, e.to_string()))?;
            if base.is_empty() {
                return Err(node_err(ctx, "Base URL is empty"));
            }
            format!(
                "{}/{}",
                base.trim_end_matches('/'),
                url_t.trim_start_matches('/')
            )
        };

        let mut req = RestRequest {
            method: rq
                .method
                .clone()
                .unwrap_or_else(|| "GET".into())
                .to_uppercase(),
            url,
            query: Vec::new(),
            headers: self.spec.default_headers.clone(),
            body: RestBody::None,
        };
        let mut body = Map::new();
        for (k, v) in &rq.qs {
            req.query.push((k.clone(), display(&ev(ctx, v, i)?)));
        }
        for (k, v) in &rq.headers {
            req.headers.push((k.clone(), display(&ev(ctx, v, i)?)));
        }
        for (k, v) in &rq.body {
            set_path(&mut body, k, ev(ctx, v, i)?);
        }

        for p in &self.desc.properties {
            if p.name == RESOURCE
                || p.name == OPERATION
                || !is_visible(p.display_options.as_ref(), &look)
            {
                continue;
            }
            if matches!(p.prop_type, PropertyType::Collection) {
                self.collect_collection(ctx, i, p, &mut req, &mut body)?;
            } else if let Some(send) = Routing::from_extra(&p.extra).and_then(|r| r.send) {
                let v = param(ctx, &self.desc, &p.name, i)?;
                self.send_one(ctx, i, &p.name, v, &send, &mut req, &mut body)?;
            }
        }
        if let Some(pname) = &rq.binary_body {
            req.body = self.file_body(ctx, i, pname, rq.multipart.then_some(&body))?;
        } else if let Some(raw) = body.remove(RAW_BODY) {
            req.body = RestBody::Json(raw);
        } else if !body.is_empty() {
            req.body = RestBody::Json(Value::Object(body));
        }

        let flag = |name: &str, default: bool| -> bool {
            look(name).map_or(default, |v| matches!(v, Value::Bool(true)))
        };
        let simplify = self.desc.property("simplify").is_some() && flag("simplify", true);
        let return_all = flag("returnAll", false);
        let limit = look("limit")
            .and_then(|v| v.as_u64())
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0);
        Ok(Plan {
            req,
            route,
            simplify,
            return_all,
            limit,
        })
    }

    /// The body of an upload: the item's file (optionally wrapped with JSON metadata as
    /// `multipart/related`).
    fn file_body(
        &self,
        ctx: &NodeContext,
        i: usize,
        name_param: &str,
        meta: Option<&Map<String, Value>>,
    ) -> Result<RestBody, FlowError> {
        use base64::Engine as _;
        let prop = param(ctx, &self.desc, name_param, i)?
            .map(|v| display(&v))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "data".to_owned());
        let bin = ctx
            .input
            .first()
            .and_then(|port| port.get(i))
            .and_then(|it| it.binary.as_ref())
            .and_then(|b| b.get(&prop))
            .ok_or_else(|| {
                node_err(
                    ctx,
                    format!("Input item {i} has no binary property `{prop}`"),
                )
            })?;
        if bin.data.is_empty() {
            return Err(node_err(
                ctx,
                format!(
                    "Binary property `{prop}` is stored outside the item and cannot be read here"
                ),
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(bin.data.as_bytes())
            .map_err(|e| node_err(ctx, format!("Binary data is not valid base64: {e}")))?;
        let ctype = if bin.mime_type.is_empty() {
            "application/octet-stream".to_owned()
        } else {
            bin.mime_type.clone()
        };
        let Some(meta) = meta else {
            return Ok(RestBody::Raw {
                content_type: ctype,
                data: bytes,
            });
        };
        let boundary = format!("hk{:x}", bytes.len() ^ 0x5bd1_e995);
        let mut data = Vec::with_capacity(bytes.len() + 512);
        data.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{}\r\n--{boundary}\r\nContent-Type: {ctype}\r\n\r\n",
                Value::Object(meta.clone())
            )
            .as_bytes(),
        );
        data.extend_from_slice(&bytes);
        data.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        Ok(RestBody::Raw {
            content_type: format!("multipart/related; boundary={boundary}"),
            data,
        })
    }

    fn collect_collection(
        &self,
        ctx: &NodeContext,
        i: usize,
        coll: &NodeProperty,
        req: &mut RestRequest,
        body: &mut Map<String, Value>,
    ) -> Result<(), FlowError> {
        for opt in coll.options.iter().flatten() {
            let Some(name) = opt.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(send) = opt
                .get("routing")
                .and_then(|r| serde_json::from_value::<Routing>(r.clone()).ok())
                .and_then(|r| r.send)
            else {
                continue;
            };
            // collection members are only sent when the user added them
            let Some(v) = ctx.eval_param_at(&format!("{}.{name}", coll.name), i)? else {
                continue;
            };
            self.send_one(ctx, i, name, Some(v), &send, req, body)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn send_one(
        &self,
        ctx: &NodeContext,
        i: usize,
        name: &str,
        value: Option<Value>,
        send: &RouteSend,
        req: &mut RestRequest,
        body: &mut Map<String, Value>,
    ) -> Result<(), FlowError> {
        let value = match &send.value {
            Some(v) => Some(ev(ctx, v, i)?),
            None => value,
        };
        let Some(mut value) = value else {
            return Ok(());
        };
        if send.skip_empty && empty(&value) {
            return Ok(());
        }
        if send.json
            && let Value::String(text) = &value
        {
            value = serde_json::from_str(text)
                .map_err(|e| node_err(ctx, format!("Parameter `{name}` is not valid JSON: {e}")))?;
        }
        let prop = send.property.clone().unwrap_or_else(|| name.to_owned());
        match send.kind {
            // `$` = the value is an object whose fields are the request body
            SendKind::Body if prop == "$" => match value {
                Value::Object(m) => body.extend(m),
                // a non-object (array) body replaces the whole body
                other => {
                    body.insert(RAW_BODY.to_owned(), other);
                }
            },
            SendKind::Body => set_path(body, &prop, value),
            SendKind::Query => req.query.push((prop, display(&value))),
            SendKind::Header => req.headers.push((prop, display(&value))),
        }
        Ok(())
    }

    async fn resolve_creds(&self, ctx: &NodeContext) -> Result<Resolved, FlowError> {
        let Some(c) = &self.spec.credential else {
            return Ok(Resolved {
                data: Map::new(),
                auth: AppliedAuth::default(),
            });
        };
        let reference = ctx.node.credentials.as_ref().and_then(|m| m.get(&c.name));
        let Some(reference) = reference else {
            if c.required {
                return Err(node_err(
                    ctx,
                    format!("No credentials of type `{}` selected", c.name),
                ));
            }
            return Ok(Resolved {
                data: self.creds.with_defaults(&c.name, &Map::new()),
                auth: AppliedAuth::default(),
            });
        };
        let resolver: &Arc<dyn CredentialResolver> = ctx
            .hooks
            .credentials
            .as_ref()
            .ok_or_else(|| node_err(ctx, "Credentials are not available in this runtime"))?;
        let raw = resolver.resolve(&c.name, reference).await?;
        let mut data = self.creds.with_defaults(&c.name, &raw);
        if crate::oauth::applies(&data) {
            let client = reqwest::Client::builder()
                .timeout(self.spec.timeout)
                .build()
                .map_err(|e| node_err(ctx, e.without_url().to_string()))?;
            let minted = crate::oauth::access_token(&client, &data)
                .await
                .map_err(|m| node_err(ctx, m))?;
            if let Some(next) = minted.new_refresh {
                // Rotated refresh token: store it, or the next run refreshes with a dead one.
                let mut patch = Map::new();
                patch.insert("refreshToken".into(), Value::String(next.clone()));
                if let Err(e) = resolver.persist(&c.name, reference, patch).await {
                    tracing::warn!(error = %e, "could not store the rotated refresh token");
                }
                data.insert("refreshToken".into(), Value::String(next));
            }
            data.insert("accessToken".into(), Value::String(minted.token));
        }
        let auth = self
            .creds
            .apply(&c.name, &data)
            .map_err(|e| node_err(ctx, e.to_string()))?;
        Ok(Resolved { data, auth })
    }

    // --------------------------------------------------------------- http

    /// The request with query, headers, auth and body applied (AWS requests are signed here).
    fn prepare(
        &self,
        ctx: &NodeContext,
        client: &reqwest::Client,
        req: &RestRequest,
        auth: &AppliedAuth,
    ) -> Result<reqwest::RequestBuilder, FlowError> {
        let url = reqwest::Url::parse(&req.url).map_err(|_| node_err(ctx, "Invalid URL"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(node_err(ctx, "Only http and https URLs are supported"));
        }
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|_| node_err(ctx, format!("Invalid HTTP method `{}`", req.method)))?;
        let mut rb = client.request(method, url);
        let query: Vec<_> = req.query.iter().chain(&auth.query).collect();
        if !query.is_empty() {
            rb = rb.query(&query);
        }
        for (k, v) in req.headers.iter().chain(&auth.headers) {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if let Some((u, p)) = &auth.basic {
            rb = rb.basic_auth(u, Some(p));
        }
        if let Some(aws) = &auth.aws {
            // Signed requests need the exact bytes, so encode the body here.
            let (ctype, bytes): (Option<String>, Vec<u8>) = match &req.body {
                RestBody::None => (None, Vec::new()),
                RestBody::Json(v) => (Some("application/json".into()), v.to_string().into_bytes()),
                RestBody::Form(p) => (
                    Some("application/x-www-form-urlencoded".into()),
                    p.iter()
                        .map(|(k, v)| format!("{}={}", form_enc(k), form_enc(v)))
                        .collect::<Vec<_>>()
                        .join("&")
                        .into_bytes(),
                ),
                RestBody::Raw { content_type, data } => (Some(content_type.clone()), data.clone()),
            };
            let mut signed_url = rb_url(&req.url, &query).map_err(|m| node_err(ctx, m))?;
            // amz: query params are sorted + encoded by the signer from the URL itself
            signed_url.set_fragment(None);
            let mut extra: Vec<(String, String)> = Vec::new();
            if let Some(c) = &ctype {
                extra.push(("content-type".into(), c.clone()));
            }
            for (k, v) in req.headers.iter() {
                if k.to_ascii_lowercase().starts_with("x-amz-") {
                    extra.push((k.clone(), v.clone()));
                }
            }
            let keys = crate::sigv4::Keys {
                access_key: aws.access_key.clone(),
                secret_key: aws.secret_key.clone(),
                region: aws.region.clone(),
                session_token: aws.session_token.clone(),
            };
            let date = crate::sigv4::amz_date(std::time::SystemTime::now());
            let signed = crate::sigv4::sign(
                &keys,
                &aws.service,
                &req.method,
                &signed_url,
                &extra,
                &bytes,
                &date,
            );
            let m = reqwest::Method::from_bytes(req.method.as_bytes())
                .map_err(|_| node_err(ctx, "Invalid HTTP method"))?;
            rb = client.request(m, signed_url);
            for (k, v) in req.headers.iter().chain(signed.iter()) {
                rb = rb.header(k.as_str(), v.as_str());
            }
            if let Some(c) = ctype {
                rb = rb.header("content-type", c);
            }
            rb = rb.body(bytes);
        } else {
            rb = match &req.body {
                RestBody::None => rb,
                RestBody::Json(v) => {
                    let mut v = v.clone();
                    if let Value::Object(o) = &mut v {
                        o.extend(auth.body.clone());
                    }
                    rb.header("content-type", "application/json")
                        .body(v.to_string())
                }
                RestBody::Form(p) => rb.form(p),
                RestBody::Raw { content_type, data } => {
                    // an operation that names its own Content-Type (Dropbox wants octet-stream) wins
                    let own = req
                        .headers
                        .iter()
                        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
                    let rb = if own {
                        rb
                    } else {
                        rb.header("content-type", content_type.as_str())
                    };
                    rb.body(data.clone())
                }
            };
        }
        Ok(rb)
    }

    async fn send_request(
        &self,
        ctx: &NodeContext,
        client: &reqwest::Client,
        req: &RestRequest,
        auth: &AppliedAuth,
    ) -> Result<Value, FlowError> {
        let rb = self.prepare(ctx, client, req, auth)?;
        let res = rb
            .send()
            .await
            // without_url: the query string may carry a credential
            .map_err(|e| node_err(ctx, format!("The request failed: {}", e.without_url())))?;
        let status = res.status();
        let text = res.text().await.map_err(|e| {
            node_err(
                ctx,
                format!("Could not read the response: {}", e.without_url()),
            )
        })?;
        let parsed = if text.trim().is_empty() {
            None
        } else {
            Some(serde_json::from_str::<Value>(&text).unwrap_or_else(|_| {
                if text.trim_start().starts_with('<') {
                    xml_to_json(&text).unwrap_or_else(|| json!({ "data": text }))
                } else {
                    json!({ "data": text })
                }
            }))
        };
        if !status.is_success() {
            return Err(node_err(ctx, self.map_error(status, parsed.as_ref())));
        }
        Ok(parsed.unwrap_or_else(|| json!({ "success": true })))
    }

    /// A file response: bytes + content type + a name guessed from the headers / URL.
    async fn send_bytes(
        &self,
        ctx: &NodeContext,
        client: &reqwest::Client,
        req: &RestRequest,
        auth: &AppliedAuth,
    ) -> Result<(Vec<u8>, String, Option<String>), FlowError> {
        let rb = self.prepare(ctx, client, req, auth)?;
        let res = rb
            .send()
            .await
            .map_err(|e| node_err(ctx, format!("The request failed: {}", e.without_url())))?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            let parsed = serde_json::from_str::<Value>(&text).ok();
            return Err(node_err(ctx, self.map_error(status, parsed.as_ref())));
        }
        let ctype = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map_or_else(
                || "application/octet-stream".to_owned(),
                |s| s.split(';').next().unwrap_or("").trim().to_owned(),
            );
        let name = res
            .headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
            .and_then(|d| d.split("filename=").nth(1))
            .map(|n| n.trim().trim_matches(['"', '\'', ';']).to_owned())
            .filter(|n| !n.is_empty());
        let bytes = res
            .bytes()
            .await
            .map_err(|e| node_err(ctx, format!("Could not read the file: {}", e.without_url())))?;
        Ok((bytes.to_vec(), ctype, name))
    }

    fn map_error(&self, status: reqwest::StatusCode, body: Option<&Value>) -> String {
        let detail = body.and_then(|b| {
            self.spec
                .error_paths
                .iter()
                .find_map(|p| match get_path(b, p) {
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
        msg
    }

    fn results(&self, body: &Value, out: Option<&RouteOutput>) -> Vec<Value> {
        let prop = out.and_then(|o| o.property.as_deref());
        if out.is_some_and(|o| o.rows)
            && let Some(Value::Array(table)) = prop.and_then(|p| get_path(body, p))
        {
            return table_rows(table);
        }
        match prop {
            Some(p) => match get_path(body, p) {
                Some(Value::Array(a)) => a.clone(),
                Some(Value::Null) | None => Vec::new(),
                Some(other) => vec![other.clone()],
            },
            None => match body {
                Value::Array(a) => a.clone(),
                other => vec![other.clone()],
            },
        }
    }

    /// All result values for one input item, paging when the operation says so.
    async fn fetch(
        &self,
        ctx: &NodeContext,
        client: &reqwest::Client,
        mut plan: Plan,
        auth: &AppliedAuth,
        i: usize,
    ) -> Result<Vec<Value>, FlowError> {
        if let Some(hook) = &self.spec.pre_send {
            hook(&mut plan.req, ctx, i)?;
        }
        let output = plan.route.output.clone();
        let is_list = output.as_ref().is_some_and(|o| o.property.is_some());
        let target = if plan.return_all || !is_list {
            usize::MAX
        } else {
            plan.limit.unwrap_or(usize::MAX)
        };
        let mut req = plan.req;
        let mut out: Vec<Value> = Vec::new();

        let Some(p) = plan.route.pagination.clone() else {
            let body = self.send_request(ctx, client, &req, auth).await?;
            out = self.results(&body, output.as_ref());
            out.truncate(target);
            return Ok(out);
        };

        let mut offset = 0usize;
        let mut page_no = match &p {
            Pagination::Page { first_page, .. } => *first_page,
            _ => 0,
        };
        let mut cursor: Option<String> = None;
        for _ in 0..self.spec.max_pages.max(1) {
            let remaining = target.saturating_sub(out.len());
            let size = match &p {
                Pagination::Offset { page_size, .. }
                | Pagination::Page { page_size, .. }
                | Pagination::Cursor { page_size, .. }
                | Pagination::LastId { page_size, .. } => (*page_size).min(remaining).max(1),
                Pagination::NextUrl { .. } => usize::MAX,
            };
            match &p {
                Pagination::Offset {
                    limit_param,
                    offset_param,
                    in_body,
                    ..
                } => {
                    set_page(&mut req, *in_body, limit_param, json!(size));
                    set_page(&mut req, *in_body, offset_param, json!(offset));
                }
                Pagination::Page {
                    page_param,
                    size_param,
                    ..
                } => {
                    set_page(&mut req, false, size_param, json!(size));
                    set_page(&mut req, false, page_param, json!(page_no));
                }
                Pagination::Cursor {
                    cursor_param,
                    size_param,
                    in_body,
                    ..
                } => {
                    if let Some(sp) = size_param {
                        set_page(&mut req, *in_body, sp, json!(size));
                    }
                    if let Some(c) = &cursor {
                        set_page(&mut req, *in_body, cursor_param, json!(c));
                    }
                }
                Pagination::LastId {
                    cursor_param,
                    size_param,
                    ..
                } => {
                    set_page(&mut req, false, size_param, json!(size));
                    if let Some(c) = &cursor {
                        set_page(&mut req, false, cursor_param, json!(c));
                    }
                }
                Pagination::NextUrl { .. } => {}
            }
            let body = self.send_request(ctx, client, &req, auth).await?;
            let got = self.results(&body, output.as_ref());
            let n = got.len();
            out.extend(got);
            if out.len() >= target || n == 0 {
                break;
            }
            match &p {
                Pagination::Offset { .. } => {
                    offset += n;
                    if n < size {
                        break;
                    }
                }
                Pagination::Page { .. } => {
                    page_no += 1;
                    if n < size {
                        break;
                    }
                }
                Pagination::Cursor { next_path, .. } => {
                    match get_path(&body, next_path)
                        .map(display)
                        .filter(|s| !s.is_empty())
                    {
                        Some(c) => cursor = Some(c),
                        None => break,
                    }
                }
                Pagination::LastId {
                    id_field,
                    more_path,
                    page_size,
                    ..
                } => {
                    let more = if more_path.is_empty() {
                        n >= (*page_size).min(size)
                    } else {
                        matches!(get_path(&body, more_path), Some(Value::Bool(true)))
                    };
                    let last = out
                        .last()
                        .and_then(|v| get_path(v, id_field))
                        .map(display)
                        .filter(|s| !s.is_empty());
                    match (more, last) {
                        (true, Some(id)) => cursor = Some(id),
                        _ => break,
                    }
                }
                Pagination::NextUrl { next_path } => {
                    let Some(next) = get_path(&body, next_path)
                        .map(display)
                        .filter(|s| !s.is_empty())
                    else {
                        break;
                    };
                    let base =
                        reqwest::Url::parse(&req.url).map_err(|_| node_err(ctx, "Invalid URL"))?;
                    req.url = base
                        .join(&next)
                        .map_err(|_| {
                            node_err(ctx, "Next URL from the response is not a valid URL")
                        })?
                        .to_string();
                    req.query.clear();
                }
            }
        }
        out.truncate(target);
        Ok(out)
    }

    /// File download: one item, the file in its binary property.
    async fn download(
        &self,
        ctx: &NodeContext,
        client: &reqwest::Client,
        mut plan: Plan,
        creds: &Resolved,
        i: usize,
    ) -> Result<Vec<Item>, FlowError> {
        use base64::Engine as _;
        if let Some(hook) = &self.spec.pre_send {
            hook(&mut plan.req, ctx, i)?;
        }
        let prop = param(ctx, &self.desc, "binaryPropertyName", i)?
            .map(|v| display(&v))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "data".to_owned());
        let (bytes, ctype, name) = self.send_bytes(ctx, client, &plan.req, &creds.auth).await?;
        let file_name = param(ctx, &self.desc, "fileName", i)?
            .map(|v| display(&v))
            .filter(|s| !s.is_empty())
            .or(name);
        let mut it = Item::from_json(json!({
            "fileName": file_name,
            "mimeType": ctype,
            "fileSize": bytes.len(),
        }));
        let mut bin = hk_flow::BinaryData {
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            mime_type: ctype,
            file_name,
            ..hk_flow::BinaryData::default()
        };
        bin.file_type = None;
        let mut map = indexmap::IndexMap::new();
        map.insert(prop, bin);
        it.binary = Some(map);
        Ok(vec![it])
    }

    async fn run_item(
        &self,
        ctx: &NodeContext,
        client: &reqwest::Client,
        creds: &Resolved,
        i: usize,
    ) -> Result<Vec<Item>, FlowError> {
        let plan = self.plan(ctx, i, creds)?;
        if plan.route.output.as_ref().is_some_and(|o| o.binary) {
            return self.download(ctx, client, plan, creds, i).await;
        }
        let (simplify, fields) = (
            plan.simplify,
            plan.route
                .output
                .as_ref()
                .map(|o| o.simplify.clone())
                .unwrap_or_default(),
        );
        let values = self.fetch(ctx, client, plan, &creds.auth, i).await?;
        Ok(values
            .into_iter()
            .map(|v| {
                let v = if simplify && !fields.is_empty() {
                    pick(&v, &fields)
                } else {
                    v
                };
                match v {
                    Value::Object(m) => Item::new(m),
                    other => Item::from_json(json!({ "value": other })),
                }
            })
            .collect())
    }
}

#[async_trait]
impl FlowNode for RestNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let carry = matches!(
            ctx.node.error_mode(),
            ErrorMode::ContinueRegularOutput | ErrorMode::ContinueErrorOutput
        );
        let input = ctx.input.first().map(Vec::as_slice).unwrap_or(&[]);
        if input.is_empty() {
            return Ok(vec![Vec::new()]);
        }
        let creds = self.resolve_creds(ctx).await?;
        let client = reqwest::Client::builder()
            .timeout(self.spec.timeout)
            .build()
            .map_err(|e| {
                node_err(
                    ctx,
                    format!("Could not build HTTP client: {}", e.without_url()),
                )
            })?;
        let mut out = Vec::new();
        for i in 0..input.len() {
            match self.run_item(ctx, &client, &creds, i).await {
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
        }
        Ok(vec![out])
    }
}
