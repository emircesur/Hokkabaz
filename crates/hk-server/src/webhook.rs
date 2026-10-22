//! n8n-shaped webhook ingress: `/webhook/{path}` (production, active workflows) and
//! `/webhook-test/{path}` (test listener, one-shot).
//!
//! The HTTP side only knows the [`WebhookHandler`] trait: look up `(mode, method, path)`,
//! claim the route, run it. [`WebhookRegistry`] is the simple in-server implementation.
//!
//! Adapter point for hk-flow (P4.4): implement [`WebhookHandler`] for a thin wrapper around
//! `hk_flow::triggers::WebhookRegistry` (translate its entry into [`WebhookRoute`]/
//! [`WebhookSpec`] in `lookup`, call its trigger in `handle`), and pass that as
//! `FlowApi::webhooks`. Nothing else in this crate has to change.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use futures::future::BoxFuture;
use hk_flow::Item;
use hk_flow::hooks::{ResponseBody, ResponseSink, WebhookResponse};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;

use crate::api::AppState;
use crate::auth::{Reject, Scope, authorize, token_from};
use crate::error::ApiError;
use crate::payload;
use crate::signature::{HmacSpec, verify};

/// n8n's default payload cap.
pub const DEFAULT_MAX_BODY: usize = 16 * 1024 * 1024;
const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebhookMode {
    Production,
    Test,
}

/// Webhook node "Respond" option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResponseMode {
    /// Answer immediately, run in the background.
    #[default]
    OnReceived,
    /// Answer with the last node's output when the workflow ends.
    LastNode,
    /// Answer with what a Respond to Webhook node sends.
    ResponseNode,
}

/// "Response Data" for [`ResponseMode::LastNode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LastNodeData {
    #[default]
    FirstEntryJson,
    AllEntries,
    NoData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookAuth {
    #[default]
    None,
    /// Caller must present a token holding `webhook:invoke`.
    Bearer,
}

/// Per-webhook behaviour. No secret values: HMAC refers to a `SecretStore` handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookSpec {
    /// Workflow that owns the webhook (for bulk removal on deactivate).
    #[serde(default)]
    pub owner: String,
    /// Path pattern; `:name` segments capture into `params`.
    pub path: String,
    /// Upper-case HTTP methods this webhook answers.
    #[serde(default = "default_methods")]
    pub methods: Vec<String>,
    #[serde(default)]
    pub response_mode: ResponseMode,
    #[serde(default)]
    pub response_data: LastNodeData,
    /// Status for `onReceived` / `lastNode` answers.
    #[serde(default = "default_code")]
    pub response_code: u16,
    #[serde(default)]
    pub hmac: Option<HmacSpec>,
    #[serde(default)]
    pub auth: WebhookAuth,
    #[serde(default = "default_max_body")]
    pub max_body_bytes: usize,
    /// Cap on waiting for `lastNode` / `responseNode` answers.
    #[serde(default = "default_timeout", with = "secs")]
    pub response_timeout: Duration,
}

fn default_methods() -> Vec<String> {
    vec!["GET".into()]
}
fn default_code() -> u16 {
    200
}
fn default_max_body() -> usize {
    DEFAULT_MAX_BODY
}
fn default_timeout() -> Duration {
    DEFAULT_RESPONSE_TIMEOUT
}

mod secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_secs(u64::deserialize(d)?))
    }
}

impl WebhookSpec {
    pub fn new(owner: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            path: path.into(),
            methods: default_methods(),
            response_mode: ResponseMode::default(),
            response_data: LastNodeData::default(),
            response_code: default_code(),
            hmac: None,
            auth: WebhookAuth::None,
            max_body_bytes: DEFAULT_MAX_BODY,
            response_timeout: DEFAULT_RESPONSE_TIMEOUT,
        }
    }
}

/// Resolved route: which registration matched and what the path captured.
#[derive(Debug, Clone)]
pub struct WebhookRoute {
    /// Opaque registration id (for [`WebhookHandler::claim`]).
    pub id: u64,
    pub spec: Arc<WebhookSpec>,
    pub params: Map<String, Value>,
}

/// Everything a run needs from one accepted request.
#[derive(Debug)]
pub struct WebhookCall {
    pub mode: WebhookMode,
    pub method: String,
    pub path: String,
    /// `{headers, params, query, body}` as the Webhook node emits it.
    pub item: Item,
    /// Unparsed body for Raw Body / binary handling.
    pub raw_body: Bytes,
    /// Where a Respond to Webhook node posts its answer (only read in `responseNode` mode).
    pub sink: Arc<dyn ResponseSink>,
}

/// Final output of the run: the last node's first-output items, or an error message
/// (no secrets, no stack).
pub type WebhookResult = Result<Vec<Item>, String>;

/// Handle on a started run. `done` may be dropped unread (`onReceived`).
#[derive(Debug)]
pub struct WebhookRun {
    pub execution_id: Option<i64>,
    pub done: oneshot::Receiver<WebhookResult>,
}

/// Lookup + dispatch seam between HTTP and whatever runs workflows.
#[async_trait]
pub trait WebhookHandler: Send + Sync + fmt::Debug {
    /// Resolve a request line. `path` has no leading/trailing slash.
    fn lookup(&self, mode: WebhookMode, method: &str, path: &str) -> Option<WebhookRoute>;

    /// Atomically take the route right before running it. Test listeners return `true`
    /// exactly once; production routes always `true`. Called after auth + signature checks so
    /// a rejected request doesn't burn the listener.
    fn claim(&self, _route: &WebhookRoute) -> bool {
        true
    }

    /// Forget everything registered for workflow `owner` (deactivate / delete).
    fn unregister_owner(&self, _owner: &str) {}

    /// Start the run. Must return quickly; the work continues in the background.
    async fn handle(&self, route: &WebhookRoute, call: WebhookCall)
    -> Result<WebhookRun, ApiError>;
}

/// How a registered target starts a run.
pub type WebhookTarget =
    Arc<dyn Fn(WebhookCall) -> BoxFuture<'static, Result<WebhookRun, ApiError>> + Send + Sync>;

struct Entry {
    id: u64,
    mode: WebhookMode,
    spec: Arc<WebhookSpec>,
    target: WebhookTarget,
    expires: Option<Instant>,
    /// Test listener already taken by a request.
    claimed: bool,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    entries: Vec<Entry>,
}

/// In-server registry. Cheap to clone.
#[derive(Clone, Default)]
pub struct WebhookRegistry {
    inner: Arc<Mutex<Inner>>,
}

impl fmt::Debug for WebhookRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.lock().entries.len();
        f.debug_struct("WebhookRegistry")
            .field("entries", &n)
            .finish()
    }
}

pub fn normalize_path(p: &str) -> String {
    p.trim_matches('/').to_owned()
}

/// Match `pattern` (with `:param` segments) against `path`. Returns captures and the number
/// of literal segments (higher = more specific).
fn match_path(pattern: &str, path: &str) -> Option<(Map<String, Value>, usize)> {
    let (ps, xs): (Vec<_>, Vec<_>) = (pattern.split('/').collect(), path.split('/').collect());
    if ps.len() != xs.len() {
        return None;
    }
    let (mut params, mut literal) = (Map::new(), 0);
    for (p, x) in ps.iter().zip(&xs) {
        if let Some(name) = p.strip_prefix(':').filter(|n| !n.is_empty()) {
            if x.is_empty() {
                return None;
            }
            params.insert(name.to_owned(), Value::String((*x).to_owned()));
        } else if p == x {
            literal += 1;
        } else {
            return None;
        }
    }
    Some((params, literal))
}

impl WebhookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register `spec` for every method it lists. Conflicts (same mode, method and
    /// path pattern) are rejected like n8n's "path already in use". Test registrations expire
    /// after `ttl` (n8n's listener window is 2 minutes).
    pub fn register(
        &self,
        mode: WebhookMode,
        mut spec: WebhookSpec,
        ttl: Option<Duration>,
        target: WebhookTarget,
    ) -> Result<u64, ApiError> {
        spec.path = normalize_path(&spec.path);
        for m in &mut spec.methods {
            m.make_ascii_uppercase();
        }
        if spec.path.is_empty() || spec.methods.is_empty() {
            return Err(ApiError::BadRequest(
                "webhook needs a path and at least one method".into(),
            ));
        }
        let now = Instant::now();
        let mut g = self.lock();
        g.entries.retain(|e| e.expires.is_none_or(|t| t > now));
        for e in &g.entries {
            if e.mode == mode
                && e.spec.path == spec.path
                && e.spec.methods.iter().any(|m| spec.methods.contains(m))
            {
                return Err(ApiError::Conflict(format!(
                    "The path \"{}\" is already registered for one of the given methods",
                    spec.path
                )));
            }
        }
        g.next_id += 1;
        let id = g.next_id;
        g.entries.push(Entry {
            id,
            mode,
            spec: Arc::new(spec),
            target,
            expires: ttl.map(|d| now + d),
            claimed: false,
        });
        Ok(id)
    }

    pub fn unregister(&self, id: u64) -> bool {
        let mut g = self.lock();
        let n = g.entries.len();
        g.entries.retain(|e| e.id != id);
        g.entries.len() != n
    }

    /// Drop every registration owned by `owner` (workflow deactivated or deleted).
    pub fn unregister_owner(&self, owner: &str) -> usize {
        let mut g = self.lock();
        let n = g.entries.len();
        g.entries.retain(|e| e.spec.owner != owner);
        n - g.entries.len()
    }

    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl WebhookHandler for WebhookRegistry {
    fn lookup(&self, mode: WebhookMode, method: &str, path: &str) -> Option<WebhookRoute> {
        let path = normalize_path(path);
        let now = Instant::now();
        let g = self.lock();
        g.entries
            .iter()
            .filter(|e| e.mode == mode && !e.claimed && e.expires.is_none_or(|t| t > now))
            .filter(|e| {
                e.spec
                    .methods
                    .iter()
                    .any(|m| m.eq_ignore_ascii_case(method))
            })
            .filter_map(|e| match_path(&e.spec.path, &path).map(|(p, lit)| (lit, e, p)))
            .max_by_key(|(lit, ..)| *lit)
            .map(|(_, e, params)| WebhookRoute {
                id: e.id,
                spec: Arc::clone(&e.spec),
                params,
            })
    }

    fn unregister_owner(&self, owner: &str) {
        WebhookRegistry::unregister_owner(self, owner);
    }

    fn claim(&self, route: &WebhookRoute) -> bool {
        let now = Instant::now();
        let mut g = self.lock();
        let Some(e) = g.entries.iter_mut().find(|e| e.id == route.id) else {
            return false;
        };
        if e.claimed || e.expires.is_some_and(|t| t <= now) {
            return false;
        }
        // One-shot: later lookups skip it; `handle` drops it once the target is taken.
        e.claimed = e.mode == WebhookMode::Test;
        true
    }

    async fn handle(
        &self,
        route: &WebhookRoute,
        call: WebhookCall,
    ) -> Result<WebhookRun, ApiError> {
        let target = {
            let mut g = self.lock();
            let i = g
                .entries
                .iter()
                .position(|e| e.id == route.id)
                .ok_or_else(|| ApiError::NotFound("webhook no longer registered".into()))?;
            let target = Arc::clone(&g.entries[i].target);
            if g.entries[i].claimed {
                g.entries.remove(i);
            }
            target
        };
        target(call).await
    }
}

// ------------------------------------------------------------------ HTTP side

/// Forwards the first Respond to Webhook answer to the waiting request; later ones are
/// dropped (a workflow can only answer once).
#[derive(Debug)]
pub struct ChannelSink(Mutex<Option<oneshot::Sender<WebhookResponse>>>);

impl ChannelSink {
    pub fn new() -> (Arc<Self>, oneshot::Receiver<WebhookResponse>) {
        let (tx, rx) = oneshot::channel();
        (Arc::new(Self(Mutex::new(Some(tx)))), rx)
    }
}

impl ResponseSink for ChannelSink {
    fn respond(&self, response: WebhookResponse) {
        if let Some(tx) = self.0.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = tx.send(response);
        }
    }
}

fn n8n_error(status: StatusCode, message: impl Into<String>) -> Response {
    // n8n's webhook error body: `{"code": <n>, "message": "..."}`.
    (
        status,
        axum::Json(json!({ "code": status.as_u16(), "message": message.into() })),
    )
        .into_response()
}

fn api_error(e: &ApiError) -> Response {
    n8n_error(e.status(), e.to_string())
}

fn not_registered(mode: WebhookMode, method: &Method, path: &str) -> Response {
    let mut body = json!({
        "code": 404,
        "message": format!("The requested webhook \"{method} {path}\" is not registered."),
    });
    if mode == WebhookMode::Test {
        body["hint"] = json!(
            "Click the 'Execute workflow' button on the canvas, then try again. \
             (In test mode, the webhook only works for one call after you click this button)"
        );
    }
    (StatusCode::NOT_FOUND, axum::Json(body)).into_response()
}

pub(crate) async fn production(
    state: State<AppState>,
    path: Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    serve(
        state.0,
        WebhookMode::Production,
        path.0,
        method,
        uri,
        headers,
        body,
    )
    .await
}

pub(crate) async fn test(
    state: State<AppState>,
    path: Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    serve(
        state.0,
        WebhookMode::Test,
        path.0,
        method,
        uri,
        headers,
        body,
    )
    .await
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn serve(
    state: AppState,
    mode: WebhookMode,
    path: String,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(flow) = state.flow.clone() else {
        return not_registered(mode, &method, &path);
    };
    let path = normalize_path(&path);
    // Lookup before touching the body: unknown hooks never cost a read.
    let Some(route) = flow.webhooks.lookup(mode, method.as_str(), &path) else {
        return not_registered(mode, &method, &path);
    };
    let spec = Arc::clone(&route.spec);

    if spec.auth == WebhookAuth::Bearer {
        match authorize(&state, token_from(&headers))
            .and_then(|p| Ok(p.require(Scope::WebhookInvoke)?))
        {
            Ok(()) => {}
            Err(Reject::Api(e)) => return api_error(&e),
            Err(r @ Reject::Throttled(_)) => return axum::response::IntoResponse::into_response(r),
        }
    }

    let too_big = || {
        n8n_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("Payload too large (limit {} bytes)", spec.max_body_bytes),
        )
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > spec.max_body_bytes) {
        return too_big();
    }
    let raw = match axum::body::to_bytes(body, spec.max_body_bytes).await {
        Ok(b) => b,
        Err(_) => return too_big(),
    };

    if let Some(h) = &spec.hmac {
        let key = match flow.secrets.get(&h.secret) {
            Ok(Some(k)) => k,
            // Fail closed; don't say which handle is missing to the caller.
            Ok(None) | Err(_) => {
                tracing::error!(owner = %spec.owner, "webhook HMAC secret handle unavailable");
                return n8n_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Webhook signature check is misconfigured",
                );
            }
        };
        if let Err(e) = verify(
            &h.preset,
            key.expose().as_bytes(),
            &headers,
            &raw,
            unix_now(),
        ) {
            tracing::warn!(owner = %spec.owner, reason = %e, "webhook signature rejected");
            return n8n_error(StatusCode::UNAUTHORIZED, "Invalid webhook signature");
        }
    }

    let body_json = match payload::parse_body(&headers, &raw) {
        Ok(b) => b,
        Err(e) => return api_error(&e),
    };

    if !flow.webhooks.claim(&route) {
        return not_registered(mode, &method, &path);
    }

    let mut json = Map::new();
    json.insert(
        "headers".into(),
        Value::Object(payload::headers_json(&headers)),
    );
    json.insert("params".into(), Value::Object(route.params.clone()));
    json.insert(
        "query".into(),
        Value::Object(payload::query_json(uri.query())),
    );
    json.insert("body".into(), body_json);

    let (sink, mut sink_rx) = ChannelSink::new();
    let call = WebhookCall {
        mode,
        method: method.as_str().to_owned(),
        path,
        item: Item::new(json),
        raw_body: raw,
        sink,
    };
    let run = match flow.webhooks.handle(&route, call).await {
        Ok(r) => r,
        Err(e) => return api_error(&e),
    };

    let ok_status = StatusCode::from_u16(spec.response_code).unwrap_or(StatusCode::OK);
    match spec.response_mode {
        ResponseMode::OnReceived => (
            ok_status,
            axum::Json(json!({ "message": "Workflow was started" })),
        )
            .into_response(),
        ResponseMode::LastNode => match tokio::time::timeout(spec.response_timeout, run.done).await
        {
            Err(_) => n8n_error(
                StatusCode::GATEWAY_TIMEOUT,
                "Workflow did not finish in time",
            ),
            Ok(Ok(Ok(items))) => last_node_response(ok_status, spec.response_data, &items),
            Ok(Ok(Err(_)) | Err(_)) => {
                n8n_error(StatusCode::INTERNAL_SERVER_ERROR, "Error in workflow")
            }
        },
        ResponseMode::ResponseNode => {
            let mut done = run.done;
            let outcome = tokio::time::timeout(spec.response_timeout, async {
                let first = tokio::select! {
                    r = &mut sink_rx => Some(r.ok()),
                    _ = &mut done => None,
                };
                // Run ended first: a response sent just before the end still counts.
                first.unwrap_or_else(|| sink_rx.try_recv().ok())
            })
            .await;
            match outcome {
                Err(_) => n8n_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    "Workflow did not respond in time",
                ),
                Ok(Some(resp)) => to_http(resp),
                // Finished without reaching a Respond to Webhook node.
                Ok(None) => (
                    StatusCode::OK,
                    axum::Json(json!({ "message": "Workflow executed successfully" })),
                )
                    .into_response(),
            }
        }
    }
}

fn last_node_response(status: StatusCode, mode: LastNodeData, items: &[Item]) -> Response {
    match mode {
        LastNodeData::NoData => status.into_response(),
        LastNodeData::AllEntries => {
            let all: Vec<Value> = items
                .iter()
                .map(|i| Value::Object(i.json.clone()))
                .collect();
            (status, axum::Json(Value::Array(all))).into_response()
        }
        LastNodeData::FirstEntryJson => match items.first() {
            Some(i) => (status, axum::Json(Value::Object(i.json.clone()))).into_response(),
            None => (
                status,
                axum::Json(json!({ "message": "Workflow executed successfully" })),
            )
                .into_response(),
        },
    }
}

/// Respond to Webhook answer -> HTTP response.
fn to_http(r: WebhookResponse) -> Response {
    let mut status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::OK);
    let (default_ct, body) = match r.body {
        ResponseBody::Empty => (None, Body::empty()),
        ResponseBody::Json(v) => (
            Some("application/json".to_owned()),
            Body::from(v.to_string()),
        ),
        ResponseBody::Text(t) => (Some("text/html; charset=utf-8".to_owned()), Body::from(t)),
        ResponseBody::Binary { mime_type, data } => (Some(mime_type), Body::from(data)),
        ResponseBody::Redirect(url) => {
            if !status.is_redirection() {
                status = StatusCode::FOUND;
            }
            let mut res = Response::new(Body::empty());
            if let Ok(v) = HeaderValue::from_str(&url) {
                res.headers_mut().insert(header::LOCATION, v);
            }
            *res.status_mut() = status;
            apply_headers(&mut res, &r.headers);
            return res;
        }
    };
    let mut res = Response::new(body);
    *res.status_mut() = status;
    if let Some(ct) = default_ct.and_then(|c| HeaderValue::from_str(&c).ok()) {
        res.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    apply_headers(&mut res, &r.headers);
    res
}

fn apply_headers(res: &mut Response, headers: &[(String, String)]) {
    for (k, v) in headers {
        // Framing is the server's business.
        if matches!(
            k.to_ascii_lowercase().as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            continue;
        }
        if let (Ok(k), Ok(v)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            res.headers_mut().insert(k, v);
        }
    }
}
