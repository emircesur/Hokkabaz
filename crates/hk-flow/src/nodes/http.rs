//! HTTP Request v4.x (reqwest, feature `http`).
//!
//! Supported: methods, query/header/body parameters (keypair or JSON), JSON / form /
//! raw / binary bodies, none + generic credential auth (basic, bearer, header, query,
//! custom), response formats (autodetect/json/text/file), full response, never-error,
//! redirect limits, timeout, proxy, basic pagination (update a parameter, next URL).
//! Not supported: multipart bodies, digest/OAuth auth, predefined credential types.
//!
//! Secrets are resolved through [`CredentialResolver`](crate::hooks::CredentialResolver),
//! used to sign the request and dropped. They never reach items or error messages;
//! transport errors are stripped of their URL because query auth lives there.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Map, Value, json};

use super::util::*;
use crate::description::NodeTypeDescription;
use crate::error::FlowError;
use crate::item::{BinaryData, Item};
use crate::node::{FlowNode, NodeContext, NodeOutput};
use crate::workflow::ErrorMode;

#[derive(Debug)]
pub struct HttpRequestNode(NodeTypeDescription);

impl Default for HttpRequestNode {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpRequestNode {
    pub fn new() -> Self {
        let methods: Vec<Value> = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]
            .iter()
            .map(|m| json!({"name": m, "value": m}))
            .collect();
        Self(describe(json!({
            "displayName": "HTTP Request",
            "name": "n8n-nodes-base.httpRequest",
            "version": [4, 4.1, 4.2],
            "defaultVersion": 4.2,
            "description": "Makes an HTTP request and returns the response data",
            "subtitle": "={{$parameter[\"method\"] + \": \" + $parameter[\"url\"]}}",
            "group": ["output"],
            "defaults": {"name": "HTTP Request", "color": "#0004F5"},
            "inputs": ["main"],
            "outputs": ["main"],
            "credentials": [
                {"name": "httpBasicAuth", "required": true, "displayOptions": {"show": {"authentication": ["genericCredentialType"], "genericAuthType": ["httpBasicAuth"]}}},
                {"name": "httpBearerAuth", "required": true, "displayOptions": {"show": {"authentication": ["genericCredentialType"], "genericAuthType": ["httpBearerAuth"]}}},
                {"name": "httpHeaderAuth", "required": true, "displayOptions": {"show": {"authentication": ["genericCredentialType"], "genericAuthType": ["httpHeaderAuth"]}}},
                {"name": "httpQueryAuth", "required": true, "displayOptions": {"show": {"authentication": ["genericCredentialType"], "genericAuthType": ["httpQueryAuth"]}}},
                {"name": "httpCustomAuth", "required": true, "displayOptions": {"show": {"authentication": ["genericCredentialType"], "genericAuthType": ["httpCustomAuth"]}}}
            ],
            "properties": [
                {"displayName": "Method", "name": "method", "type": "options", "default": "GET",
                 "options": methods},
                {"displayName": "URL", "name": "url", "type": "string", "default": "", "required": true,
                 "placeholder": "http://example.com/index.html"},
                {"displayName": "Authentication", "name": "authentication", "type": "options", "default": "none",
                 "options": [
                    {"name": "None", "value": "none"},
                    {"name": "Predefined Credential Type", "value": "predefinedCredentialType"},
                    {"name": "Generic Credential Type", "value": "genericCredentialType"}]},
                {"displayName": "Generic Auth Type", "name": "genericAuthType", "type": "options", "default": "",
                 "displayOptions": {"show": {"authentication": ["genericCredentialType"]}},
                 "options": [
                    {"name": "Basic Auth", "value": "httpBasicAuth"},
                    {"name": "Bearer Auth", "value": "httpBearerAuth"},
                    {"name": "Custom Auth", "value": "httpCustomAuth"},
                    {"name": "Header Auth", "value": "httpHeaderAuth"},
                    {"name": "Query Auth", "value": "httpQueryAuth"}]},
                {"displayName": "Send Query Parameters", "name": "sendQuery", "type": "boolean", "default": false},
                {"displayName": "Specify Query Parameters", "name": "specifyQuery", "type": "options", "default": "keypair",
                 "displayOptions": {"show": {"sendQuery": [true]}},
                 "options": [{"name": "Using Fields Below", "value": "keypair"}, {"name": "Using JSON", "value": "json"}]},
                {"displayName": "Query Parameters", "name": "queryParameters", "type": "fixedCollection", "default": {"parameters": [{"name": "", "value": ""}]},
                 "typeOptions": {"multipleValues": true},
                 "displayOptions": {"show": {"sendQuery": [true], "specifyQuery": ["keypair"]}}},
                {"displayName": "JSON", "name": "jsonQuery", "type": "json", "default": "",
                 "displayOptions": {"show": {"sendQuery": [true], "specifyQuery": ["json"]}}},
                {"displayName": "Send Headers", "name": "sendHeaders", "type": "boolean", "default": false},
                {"displayName": "Specify Headers", "name": "specifyHeaders", "type": "options", "default": "keypair",
                 "displayOptions": {"show": {"sendHeaders": [true]}},
                 "options": [{"name": "Using Fields Below", "value": "keypair"}, {"name": "Using JSON", "value": "json"}]},
                {"displayName": "Header Parameters", "name": "headerParameters", "type": "fixedCollection", "default": {"parameters": [{"name": "", "value": ""}]},
                 "typeOptions": {"multipleValues": true},
                 "displayOptions": {"show": {"sendHeaders": [true], "specifyHeaders": ["keypair"]}}},
                {"displayName": "JSON", "name": "jsonHeaders", "type": "json", "default": "",
                 "displayOptions": {"show": {"sendHeaders": [true], "specifyHeaders": ["json"]}}},
                {"displayName": "Send Body", "name": "sendBody", "type": "boolean", "default": false},
                {"displayName": "Body Content Type", "name": "contentType", "type": "options", "default": "json",
                 "displayOptions": {"show": {"sendBody": [true]}},
                 "options": [
                    {"name": "Form Urlencoded", "value": "form-urlencoded"},
                    {"name": "Form-Data", "value": "multipart-form-data"},
                    {"name": "JSON", "value": "json"},
                    {"name": "n8n Binary File", "value": "binaryData"},
                    {"name": "Raw", "value": "raw"}]},
                {"displayName": "Specify Body", "name": "specifyBody", "type": "options", "default": "keypair",
                 "displayOptions": {"show": {"sendBody": [true], "contentType": ["json", "form-urlencoded"]}},
                 "options": [{"name": "Using Fields Below", "value": "keypair"}, {"name": "Using JSON", "value": "json"}]},
                {"displayName": "Body Parameters", "name": "bodyParameters", "type": "fixedCollection", "default": {"parameters": [{"name": "", "value": ""}]},
                 "typeOptions": {"multipleValues": true},
                 "displayOptions": {"show": {"sendBody": [true], "specifyBody": ["keypair"]}}},
                {"displayName": "JSON", "name": "jsonBody", "type": "json", "default": "",
                 "displayOptions": {"show": {"sendBody": [true], "specifyBody": ["json"], "contentType": ["json"]}}},
                {"displayName": "Content Type", "name": "rawContentType", "type": "string", "default": "",
                 "displayOptions": {"show": {"sendBody": [true], "contentType": ["raw"]}}},
                {"displayName": "Body", "name": "body", "type": "string", "default": "",
                 "displayOptions": {"show": {"sendBody": [true], "contentType": ["raw"]}}},
                {"displayName": "Input Data Field Name", "name": "inputDataFieldName", "type": "string", "default": "",
                 "displayOptions": {"show": {"sendBody": [true], "contentType": ["binaryData"]}}},
                {"displayName": "Options", "name": "options", "type": "collection", "default": {},
                 "placeholder": "Add option",
                 "options": [
                    {"displayName": "Response", "name": "response", "type": "fixedCollection", "default": {}},
                    {"displayName": "Pagination", "name": "pagination", "type": "fixedCollection", "default": {}},
                    {"displayName": "Redirects", "name": "redirect", "type": "fixedCollection", "default": {}},
                    {"displayName": "Timeout", "name": "timeout", "type": "number", "default": 10000},
                    {"displayName": "Proxy", "name": "proxy", "type": "string", "default": ""},
                    {"displayName": "Ignore SSL Issues (Insecure)", "name": "allowUnauthorizedCerts", "type": "boolean", "default": false}]}
            ]
        })))
    }
}

// ---------------------------------------------------------------- request model

#[derive(Debug, Clone)]
enum Body {
    None,
    Json(Value),
    Form(Vec<(String, String)>),
    Raw { content_type: String, data: Vec<u8> },
}

#[derive(Debug, Clone)]
struct Auth {
    basic: Option<(String, String)>,
    bearer: Option<String>,
    headers: Vec<(String, String)>,
    query: Vec<(String, String)>,
    body: Map<String, Value>,
}

#[derive(Debug, Clone)]
struct Req {
    method: reqwest::Method,
    url: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Body,
}

#[derive(Debug)]
struct Resp {
    status: u16,
    reason: String,
    headers: Map<String, Value>,
    content_type: String,
    bytes: Vec<u8>,
}

fn pairs_of(ctx: &NodeContext, path: &str, i: usize) -> Result<Vec<(String, Value)>, FlowError> {
    let mut out = Vec::new();
    for p in raw_array(ctx, path) {
        let name = to_display(&ctx.eval_param(p.get("name").unwrap_or(&Value::Null), i)?);
        if name.is_empty() {
            continue;
        }
        let value = ctx.eval_param(p.get("value").unwrap_or(&Value::Null), i)?;
        out.push((name, value));
    }
    Ok(out)
}

fn json_param(ctx: &NodeContext, path: &str, i: usize) -> Result<Value, FlowError> {
    let v = param_or(ctx, path, i, Value::Null)?;
    match v {
        Value::String(s) if s.trim().is_empty() => Ok(Value::Null),
        Value::String(s) => serde_json::from_str(&s)
            .map_err(|e| node_err(ctx, format!("Invalid JSON in '{path}': {e}"))),
        other => Ok(other),
    }
}

fn text_pairs(o: &Map<String, Value>) -> Vec<(String, String)> {
    o.iter().map(|(k, v)| (k.clone(), to_display(v))).collect()
}

async fn resolve_auth(ctx: &NodeContext, i: usize) -> Result<Auth, FlowError> {
    let mut auth = Auth {
        basic: None,
        bearer: None,
        headers: Vec::new(),
        query: Vec::new(),
        body: Map::new(),
    };
    match param_str(ctx, "authentication", i, "none")?.as_str() {
        "none" => return Ok(auth),
        "genericCredentialType" => {}
        other => {
            return Err(node_err(
                ctx,
                format!("Authentication `{other}` is not supported; use a generic credential type"),
            ));
        }
    }
    let kind = param_str(ctx, "genericAuthType", i, "")?;
    let reference = ctx
        .node
        .credentials
        .as_ref()
        .and_then(|c| c.get(&kind))
        .ok_or_else(|| node_err(ctx, format!("No credentials of type `{kind}` selected")))?;
    let resolver = ctx
        .hooks
        .credentials
        .as_ref()
        .ok_or_else(|| node_err(ctx, "Credentials are not available in this runtime"))?;
    let secret = resolver.resolve(&kind, reference).await?;
    let get = |k: &str| secret.get(k).map(to_display).unwrap_or_default();
    match kind.as_str() {
        "httpBasicAuth" => auth.basic = Some((get("user"), get("password"))),
        "httpBearerAuth" => auth.bearer = Some(get("token")),
        "httpHeaderAuth" => auth.headers.push((get("name"), get("value"))),
        "httpQueryAuth" => auth.query.push((get("name"), get("value"))),
        "httpCustomAuth" => {
            let spec: Value = serde_json::from_str(&get("json"))
                .map_err(|_| node_err(ctx, "Custom auth credential is not valid JSON"))?;
            if let Some(h) = spec.get("headers").and_then(Value::as_object) {
                auth.headers.extend(text_pairs(h));
            }
            if let Some(q) = spec.get("qs").and_then(Value::as_object) {
                auth.query.extend(text_pairs(q));
            }
            if let Some(b) = spec.get("body").and_then(Value::as_object) {
                auth.body = b.clone();
            }
        }
        other => {
            return Err(node_err(
                ctx,
                format!("Auth type `{other}` is not supported"),
            ));
        }
    }
    Ok(auth)
}

fn build_req(ctx: &NodeContext, i: usize, auth: &Auth, item: &Item) -> Result<Req, FlowError> {
    let method = param_str(ctx, "method", i, "GET")?.to_uppercase();
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| node_err(ctx, format!("Invalid HTTP method `{method}`")))?;
    let url = param_str(ctx, "url", i, "")?;
    if url.trim().is_empty() {
        return Err(node_err(ctx, "Parameter 'URL' is empty"));
    }

    let mut query = Vec::new();
    if param_bool(ctx, "sendQuery", i, false)? {
        if param_str(ctx, "specifyQuery", i, "keypair")? == "json" {
            if let Value::Object(o) = json_param(ctx, "jsonQuery", i)? {
                query.extend(text_pairs(&o));
            }
        } else {
            query.extend(
                pairs_of(ctx, "queryParameters.parameters", i)?
                    .into_iter()
                    .map(|(k, v)| (k, to_display(&v))),
            );
        }
    }
    query.extend(auth.query.iter().cloned());

    let mut headers = Vec::new();
    if param_bool(ctx, "sendHeaders", i, false)? {
        if param_str(ctx, "specifyHeaders", i, "keypair")? == "json" {
            if let Value::Object(o) = json_param(ctx, "jsonHeaders", i)? {
                headers.extend(text_pairs(&o));
            }
        } else {
            headers.extend(
                pairs_of(ctx, "headerParameters.parameters", i)?
                    .into_iter()
                    .map(|(k, v)| (k, to_display(&v))),
            );
        }
    }
    headers.extend(auth.headers.iter().cloned());

    let mut body = Body::None;
    if param_bool(ctx, "sendBody", i, false)? {
        let ct = param_str(ctx, "contentType", i, "json")?;
        let json_mode = param_str(ctx, "specifyBody", i, "keypair")? == "json";
        body = match ct.as_str() {
            "json" => {
                let mut v = if json_mode {
                    json_param(ctx, "jsonBody", i)?
                } else {
                    let mut o = Map::new();
                    for (k, v) in pairs_of(ctx, "bodyParameters.parameters", i)? {
                        o.insert(k, v);
                    }
                    Value::Object(o)
                };
                if let Value::Object(o) = &mut v {
                    for (k, x) in &auth.body {
                        o.insert(k.clone(), x.clone());
                    }
                }
                Body::Json(v)
            }
            "form-urlencoded" => {
                let mut pairs: Vec<(String, String)> = if json_mode {
                    match json_param(ctx, "jsonBody", i)? {
                        Value::Object(o) => text_pairs(&o),
                        _ => Vec::new(),
                    }
                } else {
                    pairs_of(ctx, "bodyParameters.parameters", i)?
                        .into_iter()
                        .map(|(k, v)| (k, to_display(&v)))
                        .collect()
                };
                pairs.extend(text_pairs(&auth.body));
                Body::Form(pairs)
            }
            "raw" => Body::Raw {
                content_type: param_str(ctx, "rawContentType", i, "text/plain")?,
                data: param_str(ctx, "body", i, "")?.into_bytes(),
            },
            "binaryData" => {
                let prop = param_str(ctx, "inputDataFieldName", i, "data")?;
                let bin = item
                    .binary
                    .as_ref()
                    .and_then(|b| b.get(&prop))
                    .ok_or_else(|| {
                        node_err(ctx, format!("Item has no binary property `{prop}`"))
                    })?;
                let data = base64::engine::general_purpose::STANDARD
                    .decode(bin.data.as_bytes())
                    .map_err(|_| node_err(ctx, "Binary data is not valid base64"))?;
                Body::Raw {
                    content_type: if bin.mime_type.is_empty() {
                        "application/octet-stream".into()
                    } else {
                        bin.mime_type.clone()
                    },
                    data,
                }
            }
            other => {
                return Err(node_err(
                    ctx,
                    format!("Body content type `{other}` is not supported"),
                ));
            }
        };
    }
    Ok(Req {
        method,
        url,
        query,
        headers,
        body,
    })
}

fn client_for(ctx: &NodeContext, i: usize) -> Result<reqwest::Client, FlowError> {
    let follow = param_bool(ctx, "options.redirect.redirect.followRedirects", i, true)?;
    let max = param_usize(ctx, "options.redirect.redirect.maxRedirects", i, 21)?;
    let timeout = param_f64(ctx, "options.timeout", i, 300_000.0)?;
    let mut b = reqwest::Client::builder()
        .redirect(if follow {
            reqwest::redirect::Policy::limited(max)
        } else {
            reqwest::redirect::Policy::none()
        })
        .timeout(Duration::from_millis(timeout.max(1.0) as u64))
        .danger_accept_invalid_certs(param_bool(ctx, "options.allowUnauthorizedCerts", i, false)?);
    let proxy = param_str(ctx, "options.proxy", i, "")?;
    if !proxy.is_empty() {
        let p = reqwest::Proxy::all(&proxy).map_err(|_| node_err(ctx, "Invalid proxy URL"))?;
        b = b.proxy(p);
    }
    b.build().map_err(|e| {
        node_err(
            ctx,
            format!("Could not build HTTP client: {}", e.without_url()),
        )
    })
}

async fn send(
    ctx: &NodeContext,
    client: &reqwest::Client,
    auth: &Auth,
    req: &Req,
) -> Result<Resp, FlowError> {
    let url = reqwest::Url::parse(&req.url)
        .map_err(|_| node_err(ctx, format!("Invalid URL: {}", req.url)))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(node_err(ctx, "Only http and https URLs are supported"));
    }
    let mut rb = client.request(req.method.clone(), url);
    if !req.query.is_empty() {
        rb = rb.query(&req.query);
    }
    for (k, v) in &req.headers {
        rb = rb.header(k.as_str(), v.as_str());
    }
    if let Some((u, p)) = &auth.basic {
        rb = rb.basic_auth(u, Some(p));
    }
    if let Some(t) = &auth.bearer {
        rb = rb.bearer_auth(t);
    }
    rb = match &req.body {
        Body::None => rb,
        Body::Json(v) => rb
            .header("content-type", "application/json")
            .body(v.to_string()),
        Body::Form(p) => rb.form(p),
        Body::Raw { content_type, data } => rb
            .header("content-type", content_type.as_str())
            .body(data.clone()),
    };
    let res = rb
        .send()
        .await
        .map_err(|e| node_err(ctx, format!("The request failed: {}", e.without_url())))?;
    let status = res.status();
    let mut headers = Map::new();
    for (k, v) in res.headers() {
        let v = String::from_utf8_lossy(v.as_bytes()).into_owned();
        // repeated headers fold into one comma separated string, like Node does
        match headers.get_mut(k.as_str()) {
            Some(Value::String(prev)) => {
                prev.push_str(", ");
                prev.push_str(&v);
            }
            _ => {
                headers.insert(k.as_str().to_owned(), Value::String(v));
            }
        }
    }
    let content_type = headers
        .get("content-type")
        .map(to_display)
        .unwrap_or_default();
    let bytes = res
        .bytes()
        .await
        .map_err(|e| {
            node_err(
                ctx,
                format!("Reading the response failed: {}", e.without_url()),
            )
        })?
        .to_vec();
    Ok(Resp {
        status: status.as_u16(),
        reason: status.canonical_reason().unwrap_or("").to_owned(),
        headers,
        content_type,
        bytes,
    })
}

// -------------------------------------------------------------------- response

fn is_json_type(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.contains("json")
}

fn is_text_type(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.starts_with("text/")
        || ct.contains("xml")
        || ct.contains("javascript")
        || ct.contains("x-www-form-urlencoded")
}

/// Parsed body as expressions see it (`$response.body`).
fn body_value(resp: &Resp) -> Value {
    let text = String::from_utf8_lossy(&resp.bytes);
    if is_json_type(&resp.content_type) || text.trim_start().starts_with(['{', '[']) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            return v;
        }
    }
    Value::String(text.into_owned())
}

fn response_to_items(ctx: &NodeContext, i: usize, resp: &Resp) -> Result<Vec<Item>, FlowError> {
    let mut format = param_str(
        ctx,
        "options.response.response.responseFormat",
        i,
        "autodetect",
    )?;
    let full = param_bool(ctx, "options.response.response.fullResponse", i, false)?;
    let prop = param_str(
        ctx,
        "options.response.response.outputPropertyName",
        i,
        "data",
    )?;
    if format == "autodetect" {
        format = if is_json_type(&resp.content_type) {
            "json"
        } else if is_text_type(&resp.content_type) || resp.bytes.is_empty() {
            "text"
        } else {
            "file"
        }
        .into();
    }

    let mut binary = None;
    let body = match format.as_str() {
        "json" => {
            let text = String::from_utf8_lossy(&resp.bytes);
            if text.trim().is_empty() {
                Value::Null
            } else {
                serde_json::from_str::<Value>(&text).map_err(|e| {
                    node_err(
                        ctx,
                        format!("The response is not valid JSON: {e} [item {i}]"),
                    )
                })?
            }
        }
        "file" => {
            let b = BinaryData {
                data: base64::engine::general_purpose::STANDARD.encode(&resp.bytes),
                mime_type: resp
                    .content_type
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_owned(),
                file_size: Some(Value::from(resp.bytes.len())),
                ..BinaryData::default()
            };
            let mut m = indexmap::IndexMap::new();
            m.insert(prop.clone(), b);
            binary = Some(m);
            Value::Null
        }
        _ => json!({ prop.clone(): String::from_utf8_lossy(&resp.bytes) }),
    };

    if full {
        let mut o = Map::new();
        let body = if format == "file" {
            Value::Null
        } else if format == "text" {
            body.get(&prop).cloned().unwrap_or(Value::Null)
        } else {
            body
        };
        o.insert("body".into(), body);
        o.insert("headers".into(), Value::Object(resp.headers.clone()));
        o.insert("statusCode".into(), Value::from(resp.status));
        o.insert("statusMessage".into(), Value::String(resp.reason.clone()));
        let mut it = Item::new(o);
        it.binary = binary;
        return Ok(vec![it]);
    }
    let mut it = match body {
        // an array response becomes one item per element
        Value::Array(a) => {
            return Ok(a
                .into_iter()
                .map(|v| match v {
                    Value::Object(o) => Item::new(o),
                    other => Item::from_json(json!({ prop.clone(): other })),
                })
                .collect());
        }
        Value::Object(o) => Item::new(o),
        Value::Null => Item::default(),
        other => Item::from_json(json!({ prop.clone(): other })),
    };
    it.binary = binary;
    Ok(vec![it])
}

// ------------------------------------------------------------------ pagination

/// `$response` / `$pageCount` live in `$vars` for the pagination expressions.
fn rewrite_page_expr(v: &Value) -> Value {
    match v {
        Value::String(s) if s.starts_with('=') => Value::String(
            s.replace("$response", "$vars.__hk_response")
                .replace("$pageCount", "$vars.__hk_page"),
        ),
        Value::Array(a) => Value::Array(a.iter().map(rewrite_page_expr).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), rewrite_page_expr(x)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn page_scope(ctx: &NodeContext, resp: &Resp, page: usize) -> NodeContext {
    let r = json!({
        "body": body_value(resp),
        "headers": resp.headers,
        "statusCode": resp.status,
    });
    ctx.with_extra_var("__hk_response", r)
        .with_extra_var("__hk_page", Value::from(page))
}

fn upsert(list: &mut Vec<(String, String)>, k: &str, v: String) {
    match list.iter_mut().find(|(n, _)| n == k) {
        Some(e) => e.1 = v,
        None => list.push((k.to_owned(), v)),
    }
}

#[derive(Debug)]
struct Pagination {
    mode: String,
    complete_when: String,
    status_codes: Vec<u16>,
    complete_expr: Value,
    next_url: Value,
    params: Vec<Value>,
    max_requests: usize,
    interval: Duration,
}

fn read_pagination(ctx: &NodeContext, i: usize) -> Result<Option<Pagination>, FlowError> {
    let base = "options.pagination.pagination";
    let mode = param_str(ctx, &format!("{base}.paginationMode"), i, "off")?;
    if mode == "off" {
        return Ok(None);
    }
    let limit = param_bool(ctx, &format!("{base}.limitPagesFetched"), i, false)?;
    let max = param_usize(ctx, &format!("{base}.maxRequests"), i, 100)?;
    Ok(Some(Pagination {
        mode,
        complete_when: param_str(
            ctx,
            &format!("{base}.paginationCompleteWhen"),
            i,
            "responseIsEmpty",
        )?,
        status_codes: param_list(ctx, &format!("{base}.statusCodesWhenComplete"), i)?
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect(),
        complete_expr: raw_param(ctx, &format!("{base}.completeExpression"))
            .cloned()
            .unwrap_or(Value::Null),
        next_url: raw_param(ctx, &format!("{base}.nextURL"))
            .cloned()
            .unwrap_or(Value::Null),
        params: raw_array(ctx, &format!("{base}.parameters.parameters")).to_vec(),
        // a hard stop even without `limitPagesFetched`: runaway pagination must end
        max_requests: if limit {
            max.max(1)
        } else {
            max.clamp(1, 1000)
        },
        interval: Duration::from_millis(
            param_f64(ctx, &format!("{base}.requestInterval"), i, 0.0)?.max(0.0) as u64,
        ),
    }))
}

fn page_is_last(
    ctx: &NodeContext,
    p: &Pagination,
    scope: &NodeContext,
    resp: &Resp,
    i: usize,
) -> Result<bool, FlowError> {
    Ok(match p.complete_when.as_str() {
        "receiveSpecificStatusCodes" => p.status_codes.contains(&resp.status),
        "other" => {
            let v = scope.eval_param(&rewrite_page_expr(&p.complete_expr), i)?;
            matches!(v, Value::Bool(true)) || v == Value::String("true".into())
        }
        _ => match body_value(resp) {
            Value::Null => true,
            Value::Array(a) => a.is_empty(),
            Value::Object(o) => o.is_empty(),
            Value::String(s) => s.is_empty(),
            _ => {
                let _ = ctx;
                false
            }
        },
    })
}

// ------------------------------------------------------------------------ node

fn check_status(ctx: &NodeContext, i: usize, resp: &Resp) -> Result<(), FlowError> {
    let never = param_bool(ctx, "options.response.response.neverError", i, false)?;
    if (200..300).contains(&resp.status) || never {
        return Ok(());
    }
    Err(node_err(
        ctx,
        format!(
            "The service returned an error: {} {} [item {i}]",
            resp.status, resp.reason
        ),
    ))
}

async fn run_item(ctx: &NodeContext, i: usize, item: &Item) -> Result<Vec<Item>, FlowError> {
    let auth = resolve_auth(ctx, i).await?;
    let client = client_for(ctx, i)?;
    let mut req = build_req(ctx, i, &auth, item)?;
    let Some(p) = read_pagination(ctx, i)? else {
        let resp = send(ctx, &client, &auth, &req).await?;
        check_status(ctx, i, &resp)?;
        return response_to_items(ctx, i, &resp);
    };

    let mut out = Vec::new();
    let mut page = 0usize;
    loop {
        if page > 0 && !p.interval.is_zero() {
            tokio::time::sleep(p.interval).await;
        }
        let resp = send(ctx, &client, &auth, &req).await?;
        check_status(ctx, i, &resp)?;
        out.extend(response_to_items(ctx, i, &resp)?);
        page += 1;
        let scope = page_scope(ctx, &resp, page);
        if page >= p.max_requests || page_is_last(ctx, &p, &scope, &resp, i)? {
            break;
        }
        if p.mode == "responseContainsNextURL" {
            let next = scope.eval_param(&rewrite_page_expr(&p.next_url), i)?;
            let next = to_display(&next);
            if next.is_empty() || next == "undefined" {
                break;
            }
            let base = reqwest::Url::parse(&req.url).map_err(|_| node_err(ctx, "Invalid URL"))?;
            req.url = base
                .join(&next)
                .map_err(|_| node_err(ctx, "Next URL from the response is not a valid URL"))?
                .to_string();
            // the next URL carries its own query
            req.query.clear();
            req.query.extend(auth.query.iter().cloned());
        } else {
            for prm in &p.params {
                let ty = prm.get("type").and_then(Value::as_str).unwrap_or("qs");
                let name =
                    to_display(&scope.eval_param(prm.get("name").unwrap_or(&Value::Null), i)?);
                let val = scope.eval_param(
                    &rewrite_page_expr(prm.get("value").unwrap_or(&Value::Null)),
                    i,
                )?;
                if name.is_empty() {
                    continue;
                }
                match ty {
                    "qs" => upsert(&mut req.query, &name, to_display(&val)),
                    "headers" => upsert(&mut req.headers, &name, to_display(&val)),
                    _ => match &mut req.body {
                        Body::Json(Value::Object(o)) => {
                            o.insert(name, val);
                        }
                        Body::Form(f) => upsert(f, &name, to_display(&val)),
                        other => {
                            let mut o = Map::new();
                            o.insert(name, val);
                            *other = Body::Json(Value::Object(o));
                        }
                    },
                }
            }
        }
    }
    Ok(out)
}

#[async_trait]
impl FlowNode for HttpRequestNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.0
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let carry = matches!(
            ctx.node.error_mode(),
            ErrorMode::ContinueRegularOutput | ErrorMode::ContinueErrorOutput
        );
        let mut out = Vec::new();
        for (i, item) in port(ctx, 0).iter().enumerate() {
            match run_item(ctx, i, item).await {
                Ok(items) => out.extend(items.into_iter().map(|mut o| {
                    o.paired_item = paired(i);
                    o
                })),
                Err(FlowError::Node { message, .. }) if carry => {
                    let mut o = Item::from_json(json!({ "error": message }));
                    o.error = Some(json!({ "message": message }));
                    o.paired_item = paired(i);
                    out.push(o);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(vec![out])
    }
}
