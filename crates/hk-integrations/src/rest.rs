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
pub type PreSend = Arc<dyn Fn(&mut RestRequest, &NodeContext, usize) -> Result<(), FlowError> + Send + Sync>;

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
            error_paths: ["message", "error.message", "error", "errors.0.message", "detail", "error_description"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            pre_send: None,
            max_pages: 1000,
        }
    }

    pub fn credential(mut self, name: &str, required: bool) -> Self {
        self.credential = Some(CredentialUse { name: name.to_owned(), required });
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
        f: impl Fn(&mut RestRequest, &NodeContext, usize) -> Result<(), FlowError> + Send + Sync + 'static,
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
        f.debug_struct("RestNode").field("type", &self.desc.name).finish_non_exhaustive()
    }
}

impl RestNode {
    pub fn new(desc: NodeTypeDescription, spec: RestSpec, creds: &CredentialTypes) -> Self {
        Self { desc, spec, creds: creds.clone() }
    }
}

// ------------------------------------------------------------------ helpers

fn node_err(ctx: &NodeContext, message: impl Into<String>) -> FlowError {
    FlowError::Node { node: ctx.node.name.clone(), message: message.into() }
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
fn param(ctx: &NodeContext, desc: &NodeTypeDescription, name: &str, i: usize) -> Result<Option<Value>, FlowError> {
    if let Some(v) = ctx.eval_param_at(name, i)? {
        return Ok(Some(v));
    }
    Ok(desc.property(name).and_then(|p| p.default.clone()))
}

fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
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
        let slot = cur.entry(p.to_owned()).or_insert_with(|| Value::Object(Map::new()));
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
                        .ok_or_else(|| node_err(ctx, format!("Operation `{wanted}` has no request routing")));
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
            format!("{}/{}", base.trim_end_matches('/'), url_t.trim_start_matches('/'))
        };

        let mut req = RestRequest {
            method: rq.method.clone().unwrap_or_else(|| "GET".into()).to_uppercase(),
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
            if p.name == RESOURCE || p.name == OPERATION || !is_visible(p.display_options.as_ref(), &look) {
                continue;
            }
            if matches!(p.prop_type, PropertyType::Collection) {
                self.collect_collection(ctx, i, p, &mut req, &mut body)?;
            } else if let Some(send) = Routing::from_extra(&p.extra).and_then(|r| r.send) {
                let v = param(ctx, &self.desc, &p.name, i)?;
                self.send_one(ctx, i, &p.name, v, &send, &mut req, &mut body)?;
            }
        }
        if !body.is_empty() {
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
        Ok(Plan { req, route, simplify, return_all, limit })
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
            let Some(name) = opt.get("name").and_then(Value::as_str) else { continue };
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
        let Some(value) = value else { return Ok(()) };
        if send.skip_empty && empty(&value) {
            return Ok(());
        }
        let prop = send.property.clone().unwrap_or_else(|| name.to_owned());
        match send.kind {
            SendKind::Body => set_path(body, &prop, value),
            SendKind::Query => req.query.push((prop, display(&value))),
            SendKind::Header => req.headers.push((prop, display(&value))),
        }
        Ok(())
    }

    async fn resolve_creds(&self, ctx: &NodeContext) -> Result<Resolved, FlowError> {
        let Some(c) = &self.spec.credential else {
            return Ok(Resolved { data: Map::new(), auth: AppliedAuth::default() });
        };
        let reference = ctx.node.credentials.as_ref().and_then(|m| m.get(&c.name));
        let Some(reference) = reference else {
            if c.required {
                return Err(node_err(ctx, format!("No credentials of type `{}` selected", c.name)));
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
        let data = self.creds.with_defaults(&c.name, &raw);
        let auth = self.creds.apply(&c.name, &data).map_err(|e| node_err(ctx, e.to_string()))?;
        Ok(Resolved { data, auth })
    }

    // --------------------------------------------------------------- http

    async fn send_request(
        &self,
        ctx: &NodeContext,
        client: &reqwest::Client,
        req: &RestRequest,
        auth: &AppliedAuth,
    ) -> Result<Value, FlowError> {
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
        rb = match &req.body {
            RestBody::None => rb,
            RestBody::Json(v) => {
                let mut v = v.clone();
                if let Value::Object(o) = &mut v {
                    o.extend(auth.body.clone());
                }
                rb.header("content-type", "application/json").body(v.to_string())
            }
            RestBody::Form(p) => rb.form(p),
            RestBody::Raw { content_type, data } => {
                rb.header("content-type", content_type.as_str()).body(data.clone())
            }
        };
        let res = rb
            .send()
            .await
            // without_url: the query string may carry a credential
            .map_err(|e| node_err(ctx, format!("The request failed: {}", e.without_url())))?;
        let status = res.status();
        let text = res
            .text()
            .await
            .map_err(|e| node_err(ctx, format!("Could not read the response: {}", e.without_url())))?;
        let parsed = if text.trim().is_empty() {
            None
        } else {
            Some(serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({ "data": text })))
        };
        if !status.is_success() {
            return Err(node_err(ctx, self.map_error(status, parsed.as_ref())));
        }
        Ok(parsed.unwrap_or_else(|| json!({ "success": true })))
    }

    fn map_error(&self, status: reqwest::StatusCode, body: Option<&Value>) -> String {
        let detail = body.and_then(|b| {
            self.spec.error_paths.iter().find_map(|p| match get_path(b, p) {
                Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
                Some(v @ (Value::Number(_) | Value::Bool(_))) => Some(v.to_string()),
                _ => None,
            })
        });
        let mut msg = format!("The service returned an error: {} {}", status.as_u16(), status.canonical_reason().unwrap_or(""));
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
        let target = if plan.return_all || !is_list { usize::MAX } else { plan.limit.unwrap_or(usize::MAX) };
        let mut req = plan.req;
        let mut out: Vec<Value> = Vec::new();

        let Some(p) = plan.route.pagination.clone() else {
            let body = self.send_request(ctx, client, &req, auth).await?;
            out = self.results(&body, output.as_ref());
            out.truncate(target);
            return Ok(out);
        };

        let upsert = |q: &mut Vec<(String, String)>, k: &str, v: String| {
            q.retain(|(n, _)| n != k);
            q.push((k.to_owned(), v));
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
                | Pagination::Cursor { page_size, .. } => (*page_size).min(remaining).max(1),
                Pagination::NextUrl { .. } => usize::MAX,
            };
            match &p {
                Pagination::Offset { limit_param, offset_param, .. } => {
                    upsert(&mut req.query, limit_param, size.to_string());
                    upsert(&mut req.query, offset_param, offset.to_string());
                }
                Pagination::Page { page_param, size_param, .. } => {
                    upsert(&mut req.query, size_param, size.to_string());
                    upsert(&mut req.query, page_param, page_no.to_string());
                }
                Pagination::Cursor { cursor_param, size_param, .. } => {
                    if let Some(sp) = size_param {
                        upsert(&mut req.query, sp, size.to_string());
                    }
                    if let Some(c) = &cursor {
                        upsert(&mut req.query, cursor_param, c.clone());
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
                    match get_path(&body, next_path).map(display).filter(|s| !s.is_empty()) {
                        Some(c) => cursor = Some(c),
                        None => break,
                    }
                }
                Pagination::NextUrl { next_path } => {
                    let Some(next) = get_path(&body, next_path).map(display).filter(|s| !s.is_empty()) else {
                        break;
                    };
                    let base = reqwest::Url::parse(&req.url).map_err(|_| node_err(ctx, "Invalid URL"))?;
                    req.url = base
                        .join(&next)
                        .map_err(|_| node_err(ctx, "Next URL from the response is not a valid URL"))?
                        .to_string();
                    req.query.clear();
                }
            }
        }
        out.truncate(target);
        Ok(out)
    }

    async fn run_item(
        &self,
        ctx: &NodeContext,
        client: &reqwest::Client,
        creds: &Resolved,
        i: usize,
    ) -> Result<Vec<Item>, FlowError> {
        let plan = self.plan(ctx, i, creds)?;
        let (simplify, fields) = (
            plan.simplify,
            plan.route.output.as_ref().map(|o| o.simplify.clone()).unwrap_or_default(),
        );
        let values = self.fetch(ctx, client, plan, &creds.auth, i).await?;
        Ok(values
            .into_iter()
            .map(|v| {
                let v = if simplify && !fields.is_empty() { pick(&v, &fields) } else { v };
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
            .map_err(|e| node_err(ctx, format!("Could not build HTTP client: {}", e.without_url())))?;
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
