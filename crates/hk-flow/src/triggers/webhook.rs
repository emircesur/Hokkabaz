//! Webhook trigger: route registry + request -> item parsing.
//!
//! The HTTP listener lives in hk-server (P4.6); it mounts [`WebhookRegistry`] under
//! `/webhook/*` (production), `/webhook-test/*` (test) and `/webhook-waiting/*` (Wait
//! resume, see [`parse_waiting_path`]) and calls [`WebhookRegistry::prepare`] per request.
//! Nothing here touches the network.
//!
//! Item shape (n8n docs): `{ headers, params, query, body }` plus `webhookUrl` and
//! `executionMode`. Non-text bodies become binary property `data` and `body` is `{}`.

use std::sync::{PoisonError, RwLock};

use base64::Engine;
use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use crate::error::FlowError;
use crate::hooks::{ResponseBody, WebhookResponse};
use crate::item::{BinaryData, Item};
use crate::workflow::{Node, Workflow};

pub const PRODUCTION_PREFIX: &str = "webhook";
pub const TEST_PREFIX: &str = "webhook-test";
pub const WAITING_PREFIX: &str = "webhook-waiting";

pub const WEBHOOK_TYPE: &str = "n8n-nodes-base.webhook";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebhookKind {
    Production,
    Test,
}

impl WebhookKind {
    pub fn execution_mode(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Test => "test",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResponseMode {
    /// Answer immediately, run in the background.
    #[default]
    OnReceived,
    /// Answer with the last executed node's data.
    LastNode,
    /// A Respond to Webhook node answers.
    ResponseNode,
}

/// Parsed Webhook node parameters. Static values only: n8n resolves webhook parameters
/// at activation, without item context.
#[derive(Debug, Clone, PartialEq)]
pub struct WebhookSpec {
    /// Uppercase. Several when `multipleMethods` is on.
    pub methods: Vec<String>,
    /// Full route (no leading slash): `<webhookId>/<path>` when the path has `:params`
    /// (n8n rule), else just `path` (`webhookId` when the path is empty).
    pub route: String,
    pub response_mode: ResponseMode,
    pub response_code: u16,
    /// `lastNode` body: `firstEntryJson` (default), `allEntries`, `firstEntryBinary`, `noData`.
    pub response_data: String,
    /// `options.responseData` text for `onReceived`.
    pub response_text: Option<String>,
    /// Binary property for `firstEntryBinary`.
    pub binary_property: String,
    pub authentication: String,
    /// `options.rawBody`: keep the body as binary, do not parse.
    pub raw_body: bool,
}

fn raw_str<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    m.get(k).and_then(Value::as_str)
}

impl WebhookSpec {
    pub fn from_node(node: &Node) -> Result<Self, FlowError> {
        let p = &node.parameters;
        for k in ["path", "httpMethod", "responseMode"] {
            if raw_str(p, k).is_some_and(|s| s.starts_with('=')) {
                return Err(FlowError::Invalid(format!(
                    "webhook `{}`: expression in `{k}` is not supported (resolved at activation)",
                    node.name
                )));
            }
        }
        let methods: Vec<String> = match p.get("httpMethod") {
            Some(Value::String(s)) if !s.is_empty() => vec![s.to_uppercase()],
            Some(Value::Array(a)) if !a.is_empty() => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_uppercase)
                .collect(),
            _ => vec!["GET".into()],
        };
        let path = raw_str(p, "path")
            .unwrap_or("")
            .trim_matches('/')
            .to_owned();
        let id = node.webhook_id.as_deref().or(node.id.as_deref());
        let route = match (path.is_empty(), path.contains(':'), id) {
            (true, _, Some(id)) => id.to_owned(),
            (true, _, None) => {
                return Err(FlowError::Invalid(format!(
                    "webhook `{}` has neither a path nor a webhookId",
                    node.name
                )));
            }
            (false, true, Some(id)) => format!("{id}/{path}"),
            (false, _, _) => path,
        };
        let opts = p.get("options").and_then(Value::as_object);
        let opt = |k: &str| opts.and_then(|o| o.get(k));
        let response_mode = match raw_str(p, "responseMode") {
            Some("lastNode") => ResponseMode::LastNode,
            Some("responseNode") => ResponseMode::ResponseNode,
            _ => ResponseMode::OnReceived,
        };
        // n8n stores the code either as a number or as `{values: {responseCode, customCode}}`
        let response_code = match opt("responseCode") {
            Some(Value::Number(n)) => n.as_u64(),
            Some(Value::Object(o)) => o
                .get("values")
                .and_then(|v| v.get("customCode").or_else(|| v.get("responseCode")))
                .and_then(Value::as_u64),
            _ => None,
        }
        .and_then(|c| u16::try_from(c).ok())
        .unwrap_or(200);
        Ok(Self {
            methods,
            route,
            response_mode,
            response_code,
            response_data: raw_str(p, "responseData")
                .unwrap_or("firstEntryJson")
                .to_owned(),
            response_text: opt("responseData")
                .and_then(Value::as_str)
                .map(str::to_owned),
            binary_property: opt("responseBinaryPropertyName")
                .and_then(Value::as_str)
                .unwrap_or("data")
                .to_owned(),
            authentication: raw_str(p, "authentication").unwrap_or("none").to_owned(),
            raw_body: opt("rawBody").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// One registered webhook endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct WebhookRoute {
    pub workflow_id: String,
    pub node_name: String,
    pub kind: WebhookKind,
    pub spec: WebhookSpec,
}

impl WebhookRoute {
    /// Public path, with prefix: `webhook/<route>`.
    pub fn url_path(&self) -> String {
        let prefix = match self.kind {
            WebhookKind::Production => PRODUCTION_PREFIX,
            WebhookKind::Test => TEST_PREFIX,
        };
        format!("{prefix}/{}", self.spec.route)
    }
}

/// Inbound request as the HTTP layer sees it.
#[derive(Debug, Clone, Default)]
pub struct WebhookRequest {
    pub method: String,
    /// Path below the kind prefix, no query: `my/path` for `/webhook/my/path?x=1`.
    pub path: String,
    pub headers: Vec<(String, String)>,
    /// Raw query string without `?`.
    pub query: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WebhookError {
    #[error("no webhook registered for {method} /{path}")]
    NotFound { method: String, path: String },
    #[error("invalid request body: {0}")]
    BadBody(String),
}

#[derive(Debug, Clone)]
pub struct WebhookMatch {
    pub route: WebhookRoute,
    pub params: Map<String, Value>,
}

/// A request ready to run.
#[derive(Debug, Clone)]
pub struct PreparedWebhook {
    pub route: WebhookRoute,
    /// The trigger item (start item of the execution).
    pub item: Item,
    /// `Some` for `onReceived`: send it right away. `None`: wait for the execution
    /// (`lastNode`: [`last_node_response`]; `responseNode`: the `ResponseSink`).
    pub immediate: Option<WebhookResponse>,
}

#[derive(Debug, Default)]
pub struct WebhookRegistry {
    routes: RwLock<Vec<WebhookRoute>>,
    /// `https://host:port`, used for the item's `webhookUrl`.
    base_url: Option<String>,
}

fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Express-style match: `:name` segments capture. Literal segments compare exactly.
fn match_route(pattern: &str, path: &str) -> Option<Map<String, Value>> {
    let (p, a) = (segments(pattern), segments(path));
    if p.len() != a.len() {
        return None;
    }
    let mut params = Map::new();
    for (ps, s) in p.iter().zip(a) {
        match ps.strip_prefix(':') {
            Some(name) => {
                params.insert(name.to_owned(), Value::String(percent_decode(s, false)));
            }
            None if *ps == s => {}
            None => return None,
        }
    }
    Some(params)
}

impl WebhookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_base_url(mut self, base: impl Into<String>) -> Self {
        self.base_url = Some(base.into().trim_end_matches('/').to_owned());
        self
    }

    /// Register every enabled Webhook node of `wf`. All-or-nothing: a `(method, route)`
    /// already taken by another workflow fails the whole call. Re-registering the same
    /// workflow replaces its routes (re-activation).
    pub fn register_workflow(
        &self,
        wf: &Workflow,
        kind: WebhookKind,
    ) -> Result<Vec<WebhookRoute>, FlowError> {
        let wf_id = wf
            .id
            .clone()
            .ok_or_else(|| FlowError::Invalid("workflow has no id".into()))?;
        let mut fresh = Vec::new();
        for n in &wf.nodes {
            if n.node_type == WEBHOOK_TYPE && !n.is_disabled() {
                fresh.push(WebhookRoute {
                    workflow_id: wf_id.clone(),
                    node_name: n.name.clone(),
                    kind,
                    spec: WebhookSpec::from_node(n)?,
                });
            }
        }
        let mut routes = self.routes.write().unwrap_or_else(PoisonError::into_inner);
        let mut kept: Vec<WebhookRoute> = routes
            .iter()
            .filter(|r| !(r.workflow_id == wf_id && r.kind == kind))
            .cloned()
            .collect();
        for f in &fresh {
            for m in &f.spec.methods {
                let clash = kept.iter().chain(fresh.iter()).any(|o| {
                    !std::ptr::eq(o, f)
                        && o.kind == kind
                        && o.spec.methods.contains(m)
                        && segments(&o.spec.route) == segments(&f.spec.route)
                });
                if clash {
                    return Err(FlowError::Invalid(format!(
                        "webhook path `{}` ({m}) is already registered",
                        f.spec.route
                    )));
                }
            }
        }
        kept.extend(fresh.iter().cloned());
        *routes = kept;
        Ok(fresh)
    }

    pub fn unregister_workflow(&self, workflow_id: &str, kind: WebhookKind) {
        self.routes
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|r| !(r.workflow_id == workflow_id && r.kind == kind));
    }

    pub fn routes(&self) -> Vec<WebhookRoute> {
        self.routes
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Literal routes win over parameterised ones.
    pub fn resolve(&self, kind: WebhookKind, method: &str, path: &str) -> Option<WebhookMatch> {
        let routes = self.routes.read().unwrap_or_else(PoisonError::into_inner);
        let mut best: Option<(usize, WebhookMatch)> = None;
        for r in routes.iter().filter(|r| r.kind == kind) {
            if !r
                .spec
                .methods
                .iter()
                .any(|m| m.eq_ignore_ascii_case(method))
            {
                continue;
            }
            if let Some(params) = match_route(&r.spec.route, path) {
                let dynamic = params.len();
                if best.as_ref().is_none_or(|(d, _)| dynamic < *d) {
                    best = Some((
                        dynamic,
                        WebhookMatch {
                            route: r.clone(),
                            params,
                        },
                    ));
                }
            }
        }
        best.map(|(_, m)| m)
    }

    /// Match + parse. The HTTP layer then starts the execution with `item` and answers
    /// per `route.spec.response_mode`.
    pub fn prepare(
        &self,
        kind: WebhookKind,
        req: &WebhookRequest,
    ) -> Result<PreparedWebhook, WebhookError> {
        let m =
            self.resolve(kind, &req.method, &req.path)
                .ok_or_else(|| WebhookError::NotFound {
                    method: req.method.clone(),
                    path: req.path.clone(),
                })?;
        let mut item = request_to_item(req, m.params, m.route.spec.raw_body)?;
        if let Some(base) = &self.base_url {
            item.json.insert(
                "webhookUrl".into(),
                json!(format!("{base}/{}", m.route.url_path())),
            );
        }
        item.json
            .insert("executionMode".into(), json!(kind.execution_mode()));
        let immediate = (m.route.spec.response_mode == ResponseMode::OnReceived)
            .then(|| on_received(&m.route.spec));
        Ok(PreparedWebhook {
            route: m.route,
            item,
            immediate,
        })
    }
}

// ------------------------------------------------------------ request parsing

/// `{ headers, params, query, body }` (+ `binary.data` for non-text bodies).
pub fn request_to_item(
    req: &WebhookRequest,
    params: Map<String, Value>,
    raw_body: bool,
) -> Result<Item, WebhookError> {
    let mut headers = Map::new();
    for (k, v) in &req.headers {
        let k = k.to_ascii_lowercase();
        match headers.get_mut(&k) {
            // repeated headers join like node's http parser does
            Some(Value::String(prev)) => {
                prev.push_str(", ");
                prev.push_str(v);
            }
            _ => {
                headers.insert(k, Value::String(v.clone()));
            }
        }
    }
    let content_type = headers
        .get("content-type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();

    let query = req
        .query
        .as_deref()
        .map(parse_urlencoded)
        .unwrap_or_default();

    let mut binary = None;
    let body = if req.body.is_empty() {
        Value::Object(Map::new())
    } else if raw_body {
        binary = Some(binary_body(&req.body, &mime));
        Value::Object(Map::new())
    } else if mime == "application/json" || mime.ends_with("+json") {
        serde_json::from_slice(&req.body).map_err(|e| WebhookError::BadBody(e.to_string()))?
    } else if mime == "application/x-www-form-urlencoded" {
        let s = std::str::from_utf8(&req.body).map_err(|e| WebhookError::BadBody(e.to_string()))?;
        Value::Object(parse_urlencoded(s))
    } else if mime.starts_with("text/") || mime.is_empty() || mime.ends_with("xml") {
        match std::str::from_utf8(&req.body) {
            Ok(s) => Value::String(s.to_owned()),
            Err(_) => {
                binary = Some(binary_body(&req.body, &mime));
                Value::Object(Map::new())
            }
        }
    } else {
        // octet-stream, images, multipart (not parsed yet): hand over as binary
        binary = Some(binary_body(&req.body, &mime));
        Value::Object(Map::new())
    };

    let mut json = Map::new();
    json.insert("headers".into(), Value::Object(headers));
    json.insert("params".into(), Value::Object(params));
    json.insert("query".into(), Value::Object(query));
    json.insert("body".into(), body);
    let mut item = Item::new(json);
    item.binary = binary;
    Ok(item)
}

fn binary_body(body: &[u8], mime: &str) -> IndexMap<String, BinaryData> {
    let mut m = IndexMap::new();
    m.insert(
        "data".to_owned(),
        BinaryData {
            data: base64::engine::general_purpose::STANDARD.encode(body),
            mime_type: if mime.is_empty() {
                "application/octet-stream".into()
            } else {
                mime.to_owned()
            },
            file_size: Some(json!(body.len().to_string())),
            ..BinaryData::default()
        },
    );
    m
}

/// `a=1&b=x%20y&a=2` -> `{a: ["1","2"], b: "x y"}`.
pub fn parse_urlencoded(s: &str) -> Map<String, Value> {
    let mut out = Map::new();
    for pair in s.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let (k, v) = (
            percent_decode(k, true),
            Value::String(percent_decode(v, true)),
        );
        match out.get_mut(&k) {
            Some(Value::Array(a)) => a.push(v),
            Some(prev) => {
                let first = prev.take();
                *prev = Value::Array(vec![first, v]);
            }
            None => {
                out.insert(k, v);
            }
        }
    }
    out
}

fn percent_decode(s: &str, plus_is_space: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' if plus_is_space => out.push(b' '),
            b'%' if i + 2 < b.len() => match (hex(b[i + 1]), hex(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    char::from(c)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

// ------------------------------------------------------------------ responses

fn on_received(spec: &WebhookSpec) -> WebhookResponse {
    WebhookResponse {
        status: spec.response_code,
        headers: Vec::new(),
        body: match &spec.response_text {
            Some(t) if !t.is_empty() => ResponseBody::Text(t.clone()),
            _ => ResponseBody::Json(json!({"message": "Workflow was started"})),
        },
    }
}

/// Answer for `responseMode == lastNode`, built from the finished execution.
pub fn last_node_response(
    spec: &WebhookSpec,
    result: &crate::executor::ExecutionResult,
) -> WebhookResponse {
    if result.error.is_some() {
        return WebhookResponse {
            status: 500,
            headers: Vec::new(),
            body: ResponseBody::Json(json!({"message": "Error in workflow"})),
        };
    }
    let items: &[Item] = result
        .last_node_executed
        .as_deref()
        .and_then(|n| result.run_data.get(n))
        .and_then(|runs| runs.last())
        .map_or(&[], |r| r.output(0));
    let body = match spec.response_data.as_str() {
        "noData" => ResponseBody::Empty,
        "allEntries" => ResponseBody::Json(Value::Array(
            items
                .iter()
                .map(|i| Value::Object(i.json.clone()))
                .collect(),
        )),
        "firstEntryBinary" => {
            let bin = items
                .first()
                .and_then(|i| i.binary.as_ref())
                .and_then(|b| b.get(&spec.binary_property));
            match bin.and_then(|b| {
                base64::engine::general_purpose::STANDARD
                    .decode(&b.data)
                    .ok()
                    .map(|d| (b.mime_type.clone(), d))
            }) {
                Some((mime_type, data)) => ResponseBody::Binary { mime_type, data },
                None => ResponseBody::Empty,
            }
        }
        _ => match items.first() {
            Some(i) => ResponseBody::Json(Value::Object(i.json.clone())),
            None => ResponseBody::Empty,
        },
    };
    WebhookResponse {
        status: spec.response_code,
        headers: Vec::new(),
        body,
    }
}

/// `webhook-waiting/<execution id>[/<suffix>]` (prefix already stripped or not).
pub fn parse_waiting_path(path: &str) -> Option<(String, Option<String>)> {
    let p = path.trim_matches('/');
    let p = p
        .strip_prefix(WAITING_PREFIX)
        .map_or(p, |r| r.trim_start_matches('/'));
    let (id, rest) = match p.split_once('/') {
        Some((id, rest)) => (id, Some(rest.trim_matches('/').to_owned())),
        None => (p, None),
    };
    (!id.is_empty()).then(|| (id.to_owned(), rest.filter(|s| !s.is_empty())))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn wf(nodes: Value) -> Workflow {
        Workflow::from_json_value(
            json!({"id": "w1", "name": "W", "nodes": nodes, "connections": {}}),
        )
        .unwrap()
    }

    fn hook(name: &str, params: Value) -> Value {
        json!({"name": name, "type": WEBHOOK_TYPE, "typeVersion": 2, "position": [0, 0],
               "webhookId": "wid-1", "parameters": params})
    }

    fn req(method: &str, path: &str) -> WebhookRequest {
        WebhookRequest {
            method: method.into(),
            path: path.into(),
            ..WebhookRequest::default()
        }
    }

    #[test]
    fn item_shape_json_body() {
        let reg = WebhookRegistry::new().with_base_url("https://hk.example/");
        reg.register_workflow(
            &wf(json!([hook(
                "H",
                json!({"path": "orders/:id", "httpMethod": "POST"})
            )])),
            WebhookKind::Production,
        )
        .unwrap();
        // dynamic path => `<webhookId>/<path>`
        let mut r = req("POST", "wid-1/orders/42");
        r.headers = vec![
            (
                "Content-Type".into(),
                "application/json; charset=utf-8".into(),
            ),
            ("X-Tag".into(), "a".into()),
            ("x-tag".into(), "b".into()),
        ];
        r.query = Some("a=1&a=2&name=J%C3%BCrgen+K&flag".into());
        r.body = br#"{"n":1}"#.to_vec();
        let p = reg.prepare(WebhookKind::Production, &r).unwrap();
        assert_eq!(
            Value::Object(p.item.json),
            json!({
                "headers": {"content-type": "application/json; charset=utf-8", "x-tag": "a, b"},
                "params": {"id": "42"},
                "query": {"a": ["1", "2"], "name": "Jürgen K", "flag": ""},
                "body": {"n": 1},
                "webhookUrl": "https://hk.example/webhook/wid-1/orders/:id",
                "executionMode": "production"
            })
        );
        assert!(p.item.binary.is_none());
        // onReceived (default) answers right away
        let imm = p.immediate.unwrap();
        assert_eq!(imm.status, 200);
        assert_eq!(
            imm.body,
            ResponseBody::Json(json!({"message": "Workflow was started"}))
        );
    }

    #[test]
    fn body_kinds() {
        let item = |ct: &str, body: &[u8]| {
            let mut r = req("POST", "x");
            r.headers = vec![("content-type".into(), ct.into())];
            r.body = body.to_vec();
            request_to_item(&r, Map::new(), false).unwrap()
        };
        assert_eq!(
            item("application/x-www-form-urlencoded", b"a=1&b=x%20y").json["body"],
            json!({"a": "1", "b": "x y"})
        );
        assert_eq!(item("text/plain", b"hi").json["body"], json!("hi"));
        let bin = item("application/octet-stream", &[0, 1, 2]);
        assert_eq!(bin.json["body"], json!({}));
        let d = &bin.binary.unwrap()["data"];
        assert_eq!(
            (d.data.as_str(), d.mime_type.as_str()),
            ("AAEC", "application/octet-stream")
        );
        let mut bad = req("POST", "x");
        bad.headers = vec![("content-type".into(), "application/json".into())];
        bad.body = b"{".to_vec();
        assert!(matches!(
            request_to_item(&bad, Map::new(), false),
            Err(WebhookError::BadBody(_))
        ));
    }

    #[test]
    fn routing_and_conflicts() {
        let reg = WebhookRegistry::new();
        let a = wf(json!([hook(
            "A",
            json!({"path": "p/fixed", "httpMethod": "GET"})
        )]));
        reg.register_workflow(&a, WebhookKind::Production).unwrap();
        // GET only, exact path
        assert!(
            reg.resolve(WebhookKind::Production, "get", "/p/fixed/")
                .is_some()
        );
        assert!(
            reg.resolve(WebhookKind::Production, "POST", "p/fixed")
                .is_none()
        );
        assert!(reg.resolve(WebhookKind::Test, "GET", "p/fixed").is_none());
        // another workflow cannot take the same path/method
        let mut b = wf(json!([hook(
            "B",
            json!({"path": "p/fixed", "httpMethod": ["GET", "POST"]})
        )]));
        b.id = Some("w2".into());
        assert!(reg.register_workflow(&b, WebhookKind::Production).is_err());
        // the owner may re-register (re-activation)
        reg.register_workflow(&a, WebhookKind::Production).unwrap();
        assert_eq!(reg.routes().len(), 1);
        reg.unregister_workflow("w1", WebhookKind::Production);
        assert!(reg.routes().is_empty());
        // missing path falls back to the webhookId
        let r = reg
            .register_workflow(&wf(json!([hook("C", json!({}))])), WebhookKind::Test)
            .unwrap();
        assert_eq!(r[0].url_path(), "webhook-test/wid-1");
    }

    #[test]
    fn response_modes() {
        let n = |p: Value| {
            let w = wf(json!([hook("H", p)]));
            WebhookSpec::from_node(&w.nodes[0]).unwrap()
        };
        let s = n(
            json!({"path": "a", "responseMode": "lastNode", "responseData": "allEntries",
                         "options": {"responseCode": 201}}),
        );
        assert_eq!(
            (s.response_mode, s.response_code),
            (ResponseMode::LastNode, 201)
        );
        assert_eq!(
            n(json!({"path": "a", "responseMode": "responseNode"})).response_mode,
            ResponseMode::ResponseNode
        );
        assert!(
            WebhookSpec::from_node(
                &wf(json!([hook("H", json!({"path": "={{ $json.x }}"}))])).nodes[0]
            )
            .is_err()
        );
    }

    #[test]
    fn waiting_paths() {
        assert_eq!(
            parse_waiting_path("webhook-waiting/e1"),
            Some(("e1".into(), None))
        );
        assert_eq!(
            parse_waiting_path("/webhook-waiting/e1/approve/"),
            Some(("e1".into(), Some("approve".into())))
        );
        assert_eq!(parse_waiting_path("e1"), Some(("e1".into(), None)));
        assert_eq!(parse_waiting_path("webhook-waiting/"), None);
    }
}
