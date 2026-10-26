//! [`RemoteCore`]: [`CoreHandle`] over a `hokkabazd` (hk-server `/api/v1` REST + `/api/v1/ws`).
//!
//! Commands go over REST with the bearer token; the live feed is one WebSocket per
//! `session_events` stream. A dropped socket reconnects with exponential backoff and replays
//! the durable events it missed from `GET /sessions/{id}?after_seq=<last seen>`; streaming
//! deltas inside the gap are gone, the durable events that follow are the source of truth.
//!
//! TLS is rustls (ring) with the bundled webpki roots on both transports, so nothing here
//! needs openssl and the crate keeps building for Android. Provider settings live on the
//! instance and are not editable through the API: those methods return
//! [`HkErrorKind::Unsupported`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::{self, BoxStream};
use futures::{SinkExt, StreamExt};
use hk_llm::{CancellationToken, ModelInfo};
use hk_server::{
    ApprovalDecision, CreateSession, ServerEvent, SessionInfo, TurnAccepted, TurnMode, TurnRequest,
};
use hk_store::Secret;
use reqwest::{Method, Url};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::dto::{
    CoreProfile, CreateSessionDto, FlowRunDetailDto, FlowRunEventDto, FlowRunEventKind,
    FlowRunModeDto, FlowRunSummaryDto, InstanceStateDto, InstanceStatusDto, ModeDto, ModelListDto,
    PreferencesDto, ProviderErrorDto, ProviderInputDto, SessionDetailDto, SessionEventDto,
    SessionEventKind, SessionInfoDto, SettingsDto, TurnAcceptedDto, TurnRequestDto,
};
use crate::error::{HkError, HkErrorKind};
use crate::flow_runs::{self, DETAIL_MAX_BYTES, DETAIL_MAX_ITEMS, MAX_INPUT_ITEMS};
use crate::handle::CoreHandle;
use crate::local::{message_dto, pending_approvals, session_dto};
use crate::{events, settings};

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PING_EVERY: Duration = Duration::from_secs(25);
/// No frame (pongs count) for this long = half-open socket.
const IDLE_LIMIT: Duration = Duration::from_secs(70);
const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(15);
const FEED_BUFFER: usize = 256;
const FEED_ATTACH_WAIT: Duration = Duration::from_secs(3);
/// Id of the subscribe command whose ack marks the socket as filtered to one session.
const SUB_ID: &str = "sub";
/// Studio calls (transcribe, analyze, render setup) run for minutes.
const STUDIO_RPC_TIMEOUT: Duration = Duration::from_secs(3600);
const STUDIO_TICKET_REFRESH: Duration = Duration::from_secs(50 * 60);

struct Inner {
    base: Url,
    token: Secret,
    http: reqwest::Client,
    profile: CoreProfile,
    shutdown: CancellationToken,
    /// Feeds still attaching, per session: commands wait for them so a turn submitted right after
    /// `session_events` cannot start before the socket listens.
    feeds: Mutex<Vec<(String, watch::Receiver<bool>)>>,
    /// App workflow id -> id of its copy on the instance (see [`Inner::ensure_workflow`]).
    flow_ids: Mutex<std::collections::HashMap<String, String>>,
    /// Media ticket for Studio players (`/studio/media?ticket=`), renewed before it expires.
    studio_ticket: Mutex<Option<(String, Instant)>>,
}

/// Remote [`CoreHandle`]. Cheap to share via `Arc`.
pub struct RemoteCore {
    i: Arc<Inner>,
}

impl std::fmt::Debug for RemoteCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteCore")
            .field("base", &self.i.base.as_str())
            .finish_non_exhaustive()
    }
}

/// Validate and normalise an instance URL: http(s) only, no query/fragment, no trailing slash.
pub fn parse_base_url(raw: &str) -> Result<Url, HkError> {
    let mut url =
        Url::parse(raw.trim()).map_err(|e| HkError::invalid(format!("invalid URL: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(HkError::invalid("URL must start with http:// or https://"));
    }
    if url.host_str().is_none() {
        return Err(HkError::invalid("URL has no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(HkError::invalid(
            "URL must not carry credentials; use the token field",
        ));
    }
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url)
}

fn http_client(timeout: Duration) -> Result<reqwest::Client, HkError> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .user_agent(concat!("hokkabaz-app/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| HkError::internal(format!("http client: {e}")))
}

fn net_err(e: reqwest::Error) -> HkError {
    let e = e.without_url();
    let kind = if e.is_connect() || e.is_timeout() {
        HkErrorKind::Unavailable
    } else {
        HkErrorKind::Internal
    };
    HkError::new(kind, format!("instance request failed: {e}"))
}

/// Map a non-2xx reply (`{"error":{"code","message"}}`) to an [`HkError`].
async fn check(res: reqwest::Response) -> Result<reqwest::Response, HkError> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    let body: Value = res.json().await.unwrap_or(Value::Null);
    let message = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map_or_else(|| format!("instance answered {status}"), str::to_owned);
    let kind = match status.as_u16() {
        401 | 403 => HkErrorKind::Unauthorized,
        404 => HkErrorKind::NotFound,
        400 | 422 => HkErrorKind::InvalidArgument,
        409 => HkErrorKind::Conflict,
        429 | 502..=504 => HkErrorKind::Unavailable,
        _ => HkErrorKind::Internal,
    };
    Err(HkError::new(kind, message))
}

#[derive(serde::Deserialize)]
struct SettingsReply {
    settings: hk_providers::SettingsView,
}

/// Older instances have no provider API (404): say so instead of "not found".
fn providers_unavailable(e: HkError) -> HkError {
    if e.kind == HkErrorKind::NotFound {
        return HkError::new(
            HkErrorKind::Unsupported,
            "this instance is too old to manage providers from the app; update it",
        );
    }
    e
}

impl Inner {
    /// `<base>/api/v1/<segments..>`; segments are percent-encoded.
    fn url(&self, segments: &[&str]) -> Url {
        let mut u = self.base.clone();
        if let Ok(mut s) = u.path_segments_mut() {
            s.pop_if_empty().extend(["api", "v1"]).extend(segments);
        }
        u
    }

    fn request(&self, method: Method, segments: &[&str]) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url(segments))
            .bearer_auth(self.token.expose())
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::Response, HkError> {
        check(rb.send().await.map_err(net_err)?).await
    }

    async fn json<T: DeserializeOwned>(&self, rb: reqwest::RequestBuilder) -> Result<T, HkError> {
        self.send(rb)
            .await?
            .json()
            .await
            .map_err(|e| HkError::internal(format!("bad reply from instance: {}", e.without_url())))
    }

    async fn post_json(
        &self,
        segments: &[&str],
        body: &impl Serialize,
    ) -> Result<reqwest::Response, HkError> {
        self.send(self.request(Method::POST, segments).json(body))
            .await
    }

    fn register_feed(&self, sid: &str) -> watch::Sender<bool> {
        let (tx, rx) = watch::channel(false);
        let mut feeds = self.feeds.lock().unwrap_or_else(PoisonError::into_inner);
        // A finished pump drops its sender; forget those.
        feeds.retain(|(_, r)| r.has_changed().is_ok());
        feeds.push((sid.to_owned(), rx));
        tx
    }

    /// Wait (bounded) until every feed of `sid` is attached. Offline instance: the command
    /// proceeds after the bound and reports its own error.
    async fn feeds_ready(&self, sid: &str) {
        let pending: Vec<_> = {
            let feeds = self.feeds.lock().unwrap_or_else(PoisonError::into_inner);
            feeds
                .iter()
                .filter(|(s, r)| s == sid && !*r.borrow())
                .map(|(_, r)| r.clone())
                .collect()
        };
        for mut rx in pending {
            let _ = tokio::time::timeout(FEED_ATTACH_WAIT, rx.wait_for(|ready| *ready)).await;
        }
    }

    async fn detail(&self, id: &str, after_seq: Option<u64>) -> Result<WireDetail, HkError> {
        let mut rb = self.request(Method::GET, &["sessions", id]);
        if let Some(n) = after_seq {
            rb = rb.query(&[("after_seq", n)]);
        }
        self.json(rb).await
    }
}

/// `GET /sessions/{id}` reply. Events stay raw JSON so one kind this build does not know
/// (newer daemon) cannot fail the whole read.
#[derive(serde::Deserialize)]
struct WireDetail {
    session: SessionInfo,
    #[serde(default)]
    events: Vec<Value>,
    #[serde(default)]
    messages: Vec<hk_llm::Message>,
    #[serde(default)]
    pending_approvals: Vec<String>,
}

impl RemoteCore {
    fn settings_dto(&self, v: &hk_providers::SettingsView) -> SettingsDto {
        SettingsDto {
            providers: settings::provider_dtos(v),
            default_model: v.default_model.clone(),
            mode: settings::mode(v.plan_mode),
            // The instance owns its workspace and data dir.
            workspace_root: v.workspace_root.clone().unwrap_or_default(),
            profile: self.i.profile,
            data_dir: String::new(),
        }
    }

    /// `token` is the instance's bearer. Nothing is contacted until the first call.
    pub fn new(base_url: &str, token: Secret, profile: CoreProfile) -> Result<Arc<Self>, HkError> {
        Ok(Arc::new(Self {
            i: Arc::new(Inner {
                base: parse_base_url(base_url)?,
                token,
                http: http_client(HTTP_TIMEOUT)?,
                profile,
                shutdown: CancellationToken::new(),
                feeds: Mutex::default(),
                flow_ids: Mutex::default(),
                studio_ticket: Mutex::default(),
            }),
        }))
    }
}

/// Reachability + auth check: `/health` (open) for version and latency, then an authenticated
/// read to tell a wrong token from a healthy instance. `id` is echoed into the result.
pub async fn probe(id: &str, base_url: &str, token: Option<&Secret>) -> InstanceStatusDto {
    let status = |state, version, latency_ms, message| InstanceStatusDto {
        id: id.to_owned(),
        state,
        version,
        latency_ms,
        message,
    };
    let (base, http) =
        match parse_base_url(base_url).and_then(|b| Ok((b, http_client(PROBE_TIMEOUT)?))) {
            Ok(x) => x,
            Err(e) => return status(InstanceStateDto::Unreachable, None, None, Some(e.message)),
        };
    let inner = Inner {
        base,
        token: token.cloned().unwrap_or_else(|| Secret::new("")),
        http,
        profile: CoreProfile::Desktop,
        shutdown: CancellationToken::new(),
        feeds: Mutex::default(),
        flow_ids: Mutex::default(),
        studio_ticket: Mutex::default(),
    };
    let started = Instant::now();
    let health: Result<Value, HkError> = inner.json(inner.http.get(inner.url(&["health"]))).await;
    let latency = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let health = match health {
        Ok(h) => h,
        Err(e) => return status(InstanceStateDto::Unreachable, None, None, Some(e.message)),
    };
    let version = health
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if token.is_none() {
        return status(
            InstanceStateDto::Unauthorized,
            version,
            Some(latency),
            Some("no token stored".into()),
        );
    }
    match inner
        .send(
            inner
                .request(Method::GET, &["sessions"])
                .query(&[("limit", 1)]),
        )
        .await
    {
        Ok(_) => status(InstanceStateDto::Online, version, Some(latency), None),
        Err(e) if e.kind == HkErrorKind::Unauthorized => status(
            InstanceStateDto::Unauthorized,
            version,
            Some(latency),
            Some(e.message),
        ),
        Err(e) => status(
            InstanceStateDto::Unreachable,
            version,
            Some(latency),
            Some(e.message),
        ),
    }
}

/// Redeem a pairing token (`POST /api/v1/pairing/exchange`, no bearer) for the instance's API
/// token of the scope the QR was minted with.
pub async fn exchange_pairing(base_url: &str, pairing_token: &str) -> Result<Secret, HkError> {
    #[derive(serde::Deserialize)]
    struct Exchanged {
        token: String,
    }
    let inner = Inner {
        base: parse_base_url(base_url)?,
        token: Secret::new(""),
        http: http_client(HTTP_TIMEOUT)?,
        profile: CoreProfile::Desktop,
        shutdown: CancellationToken::new(),
        feeds: Mutex::default(),
        flow_ids: Mutex::default(),
        studio_ticket: Mutex::default(),
    };
    let rb = inner
        .http
        .post(inner.url(&["pairing", "exchange"]))
        .json(&json!({ "token": pairing_token }));
    match inner.json::<Exchanged>(rb).await {
        Ok(x) => Ok(Secret::new(x.token)),
        // The server answers every bad token the same way; say what the user can act on.
        Err(e) if e.kind == HkErrorKind::Unauthorized => Err(HkError::new(
            HkErrorKind::Unauthorized,
            "pairing link is invalid, expired or already used",
        )),
        Err(e) if e.kind == HkErrorKind::NotFound => Err(HkError::new(
            HkErrorKind::Unsupported,
            "this instance has pairing disabled",
        )),
        Err(e) => Err(e),
    }
}

#[async_trait]
impl CoreHandle for RemoteCore {
    fn profile(&self) -> CoreProfile {
        self.i.profile
    }

    async fn get_settings(&self) -> Result<SettingsDto, HkError> {
        let r: SettingsReply = self
            .i
            .json(self.i.request(Method::GET, &["settings"]))
            .await
            .map_err(providers_unavailable)?;
        Ok(self.settings_dto(&r.settings))
    }

    async fn save_provider(&self, input: ProviderInputDto) -> Result<SettingsDto, HkError> {
        let rb = self
            .i
            .request(Method::PUT, &["providers"])
            .json(&settings::input(input));
        let r: SettingsReply = self.i.json(rb).await?;
        Ok(self.settings_dto(&r.settings))
    }

    async fn remove_provider(&self, id: String) -> Result<SettingsDto, HkError> {
        let r: SettingsReply = self
            .i
            .json(self.i.request(Method::DELETE, &["providers", &id]))
            .await?;
        Ok(self.settings_dto(&r.settings))
    }

    async fn set_api_key(
        &self,
        provider_id: String,
        key: Option<String>,
    ) -> Result<SettingsDto, HkError> {
        let rb = self
            .i
            .request(Method::PUT, &["providers", &provider_id, "key"])
            .json(&json!({ "key": key }));
        let r: SettingsReply = self.i.json(rb).await?;
        Ok(self.settings_dto(&r.settings))
    }

    async fn set_preferences(&self, prefs: PreferencesDto) -> Result<SettingsDto, HkError> {
        let rb = self
            .i
            .request(Method::PUT, &["settings", "preferences"])
            .json(&json!({
                "default_model": prefs.default_model,
                "plan_mode": matches!(prefs.mode, crate::dto::ModeDto::Plan),
            }));
        let r: SettingsReply = self.i.json(rb).await?;
        Ok(self.settings_dto(&r.settings))
    }

    async fn list_models(&self) -> Result<ModelListDto, HkError> {
        // Newer instances report which providers failed; older ones only list models.
        match self
            .i
            .json::<hk_providers::ModelsView>(self.i.request(Method::GET, &["providers", "models"]))
            .await
        {
            Ok(m) => Ok(ModelListDto {
                models: m.models.iter().map(settings::model_dto).collect(),
                errors: m
                    .errors
                    .into_iter()
                    .map(|(provider_id, message)| ProviderErrorDto {
                        provider_id,
                        message,
                    })
                    .collect(),
            }),
            Err(e) if e.kind == HkErrorKind::NotFound => {
                #[derive(serde::Deserialize)]
                struct Models {
                    models: Vec<ModelInfo>,
                }
                let m: Models = self
                    .i
                    .json(self.i.request(Method::GET, &["models"]))
                    .await?;
                Ok(ModelListDto {
                    models: m.models.iter().map(settings::model_dto).collect(),
                    errors: Vec::new(),
                })
            }
            Err(e) => Err(e),
        }
    }

    async fn refresh_catalog(&self) -> Result<(), HkError> {
        self.i
            .send(self.i.request(Method::POST, &["providers", "catalog"]))
            .await
            .map_err(providers_unavailable)?;
        Ok(())
    }

    async fn create_session(&self, req: CreateSessionDto) -> Result<SessionInfoDto, HkError> {
        let body = CreateSession {
            cwd: req.workspace_root,
            model: req.model,
            title: req.title,
        };
        let info: SessionInfo = self
            .i
            .post_json(&["sessions"], &body)
            .await?
            .json()
            .await
            .map_err(bad_reply)?;
        Ok(session_dto(info))
    }

    async fn list_sessions(&self, limit: u32, offset: u32) -> Result<Vec<SessionInfoDto>, HkError> {
        #[derive(serde::Deserialize)]
        struct Page {
            sessions: Vec<SessionInfo>,
        }
        let rb = self
            .i
            .request(Method::GET, &["sessions"])
            .query(&[("limit", limit), ("offset", offset)]);
        let p: Page = self.i.json(rb).await?;
        Ok(p.sessions.into_iter().map(session_dto).collect())
    }

    async fn get_session(&self, session_id: &str) -> Result<SessionDetailDto, HkError> {
        let d = self.i.detail(session_id, None).await?;
        let logged: Vec<hk_session::SessionEvent> = d
            .events
            .into_iter()
            .filter_map(|e| serde_json::from_value(e).ok())
            .collect();
        Ok(SessionDetailDto {
            session: session_dto(d.session),
            messages: d.messages.into_iter().map(message_dto).collect(),
            pending_approvals: pending_approvals(&logged, &d.pending_approvals),
        })
    }

    async fn submit_turn(
        &self,
        session_id: &str,
        req: TurnRequestDto,
    ) -> Result<TurnAcceptedDto, HkError> {
        if req.prompt.trim().is_empty() {
            return Err(HkError::invalid("prompt must not be empty"));
        }
        self.i.feeds_ready(session_id).await;
        let body = TurnRequest { images: Vec::new(),
            prompt: req.prompt,
            mode: req.mode.map(|m| match m {
                ModeDto::Build => TurnMode::Build,
                ModeDto::Plan => TurnMode::Plan,
            }),
            model: req.model,
        };
        let a: TurnAccepted = self
            .i
            .post_json(&["sessions", session_id, "turns"], &body)
            .await?
            .json()
            .await
            .map_err(bad_reply)?;
        Ok(TurnAcceptedDto {
            session_id: a.session_id,
            queued: a.queued,
        })
    }

    async fn inject(&self, session_id: &str, text: String) -> Result<(), HkError> {
        self.i.feeds_ready(session_id).await;
        self.i
            .post_json(
                &["sessions", session_id, "inject"],
                &json!({ "text": text }),
            )
            .await
            .map(drop)
    }

    async fn cancel(&self, session_id: &str) -> Result<(), HkError> {
        self.i
            .send(
                self.i
                    .request(Method::POST, &["sessions", session_id, "cancel"]),
            )
            .await
            .map(drop)
    }

    async fn respond_approval(
        &self,
        session_id: &str,
        approval_id: &str,
        approved: bool,
        reason: Option<String>,
    ) -> Result<(), HkError> {
        let body = ApprovalDecision {
            approved,
            reason,
            session_id: Some(session_id.to_owned()),
        };
        self.i
            .post_json(&["approvals", approval_id], &body)
            .await
            .map(drop)
    }

    fn session_events(
        &self,
        session_id: &str,
    ) -> Result<BoxStream<'static, SessionEventDto>, HkError> {
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(HkError::internal("session_events needs a tokio runtime"));
        }
        // Spawn now (not on first poll) so the socket is already subscribing while Dart goes
        // on to `submit_turn`.
        let (tx, rx) = mpsc::channel(FEED_BUFFER);
        let ready = self.i.register_feed(session_id);
        tokio::spawn(pump(self.i.clone(), session_id.to_owned(), tx, ready));
        Ok(stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (e, rx)) }).boxed())
    }

    async fn run_workflow(
        &self,
        workflow_json: String,
        mode: FlowRunModeDto,
        trigger_node: Option<String>,
        input_items_json: Option<String>,
        pin_data_json: Option<String>,
    ) -> Result<String, HkError> {
        let mut body = serde_json::Map::new();
        if let Some(n) = trigger_node {
            body.insert("triggerNode".into(), json!(n));
        }
        if let Some(items) = input_items_json.filter(|s| !s.trim().is_empty()) {
            let v: Value = serde_json::from_str(&items)
                .map_err(|e| HkError::invalid(format!("input items JSON: {e}")))?;
            if !v.as_array().is_some_and(|a| a.len() <= MAX_INPUT_ITEMS) {
                return Err(HkError::invalid(format!(
                    "input items must be an array of up to {MAX_INPUT_ITEMS} objects"
                )));
            }
            body.insert("items".into(), v);
        }
        // `Manual` ignores pins: an empty override beats the pinData stored with the uploaded copy.
        let pin = match (mode, pin_data_json.filter(|s| !s.trim().is_empty())) {
            (FlowRunModeDto::Manual, _) => Some(json!({})),
            (FlowRunModeDto::Test, Some(p)) => Some(
                serde_json::from_str::<Value>(&p)
                    .map_err(|e| HkError::invalid(format!("pin data JSON: {e}")))?,
            ),
            (FlowRunModeDto::Test, None) => None,
        };
        if let Some(p) = pin {
            body.insert("pinData".into(), p);
        }
        let rid = self.i.ensure_workflow(&workflow_json).await?;
        // The reply comes when the run is done; long runs are the instance's 120 s limit, not ours.
        let rb = self
            .i
            .request(Method::POST, &["workflows", &rid, "run"])
            .timeout(RUN_TIMEOUT)
            .json(&Value::Object(body));
        let reply: Value = self.i.json(rb).await?;
        reply
            .get("executionId")
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
            .ok_or_else(|| HkError::internal("instance answered a run without an execution id"))
    }

    fn run_events(&self, run_id: &str) -> Result<BoxStream<'static, FlowRunEventDto>, HkError> {
        let (i, id) = (self.i.clone(), run_id.to_owned());
        // The instance runs to completion before answering, so the history is the whole story.
        Ok(stream::once(async move {
            match i.run_detail(&id).await {
                Ok(d) => flow_runs::events_from_detail(&d),
                Err(e) => {
                    let mut ev = FlowRunEventDto::new(&id, FlowRunEventKind::RunError);
                    ev.status = Some("error".into());
                    ev.error = Some(e.message);
                    vec![ev]
                }
            }
        })
        .flat_map(stream::iter)
        .boxed())
    }

    async fn list_runs(
        &self,
        workflow_id: Option<String>,
        limit: u32,
    ) -> Result<Vec<FlowRunSummaryDto>, HkError> {
        let limit = limit.clamp(1, 250).to_string();
        let mut q = vec![("limit", limit)];
        if let Some(w) = workflow_id {
            // The app's id is not the instance's: translate through the upload map.
            let rid = lock(&self.i.flow_ids).get(&w).cloned().unwrap_or(w);
            q.push(("workflowId", rid));
        }
        let page: Value = self
            .i
            .json(self.i.request(Method::GET, &["executions"]).query(&q))
            .await?;
        let names = self.i.workflow_names().await;
        Ok(page
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|e| execution_summary(e, &names, None, None))
            .collect())
    }

    async fn get_run(&self, run_id: &str) -> Result<FlowRunDetailDto, HkError> {
        self.i.run_detail(run_id).await
    }

    async fn set_workflow_active(
        &self,
        workflow_json: String,
        active: bool,
    ) -> Result<bool, HkError> {
        let rid = self.i.ensure_workflow(&workflow_json).await?;
        let verb = if active { "activate" } else { "deactivate" };
        let wf: Value = self
            .i
            .json(self.i.request(Method::POST, &["workflows", &rid, verb]))
            .await?;
        Ok(wf.get("active").and_then(Value::as_bool).unwrap_or(active))
    }

    // ------------------------------------------------------------------ Studio

    fn studio_is_remote(&self) -> bool {
        true
    }

    async fn studio_rpc(&self, method: &str, params: Value) -> Result<Value, HkError> {
        let rb = self
            .i
            .request(Method::POST, &["studio", "rpc", method])
            .timeout(STUDIO_RPC_TIMEOUT)
            .json(&params);
        let mut reply: Value = self.i.json(rb).await?;
        Ok(reply
            .get_mut("result")
            .map(Value::take)
            .unwrap_or(Value::Null))
    }

    async fn studio_events(&self, id: &str) -> Result<BoxStream<'static, String>, HkError> {
        let (tx, rx) = mpsc::channel::<String>(FEED_BUFFER);
        let (i, id) = (self.i.clone(), id.to_owned());
        tokio::spawn(async move {
            let mut backoff = BACKOFF_MIN;
            loop {
                let rb = i
                    .request(Method::GET, &["studio", "events", &id])
                    .timeout(Duration::from_secs(24 * 3600));
                match rb.send().await.map_err(net_err) {
                    Ok(res) if res.status().is_success() => {
                        backoff = BACKOFF_MIN;
                        let mut bytes = res.bytes_stream();
                        let mut buf = String::new();
                        while let Some(chunk) = tokio::select! { () = i.shutdown.cancelled() => None, c = bytes.next() => c }
                        {
                            let Ok(chunk) = chunk else { break };
                            buf.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
                            while let Some(end) = buf.find("\n\n") {
                                let block: String = buf.drain(..end + 2).collect();
                                let data: Vec<&str> = block
                                    .lines()
                                    .filter_map(|l| l.strip_prefix("data:"))
                                    .map(str::trim_start)
                                    .collect();
                                if !data.is_empty() && tx.send(data.join("\n")).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    // The project is gone or the token is wrong: retrying will not help.
                    Ok(res) if matches!(res.status().as_u16(), 401 | 403 | 404) => return,
                    _ => {}
                }
                if i.shutdown.is_cancelled() {
                    return;
                }
                // Events may have been missed while disconnected.
                if tx.send(r#"{"kind":"resync"}"#.to_owned()).await.is_err() {
                    return;
                }
                tokio::select! { () = i.shutdown.cancelled() => return, () = tokio::time::sleep(backoff) => {} }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        });
        Ok(stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|m| (m, rx)) }).boxed())
    }

    async fn studio_media(&self, path: &str) -> Result<String, HkError> {
        let cached = lock(&self.i.studio_ticket)
            .clone()
            .filter(|(_, at)| at.elapsed() < STUDIO_TICKET_REFRESH);
        let ticket = match cached {
            Some((t, _)) => t,
            None => {
                let reply: Value = self
                    .i
                    .json(self.i.request(Method::POST, &["studio", "ticket"]))
                    .await?;
                let t = reply
                    .get("ticket")
                    .and_then(Value::as_str)
                    .ok_or_else(|| HkError::internal("instance sent no media ticket"))?
                    .to_owned();
                *lock(&self.i.studio_ticket) = Some((t.clone(), Instant::now()));
                t
            }
        };
        let mut url = self.i.url(&["studio", "media"]);
        url.query_pairs_mut()
            .append_pair("path", path)
            .append_pair("ticket", &ticket);
        Ok(url.into())
    }

    async fn studio_upload(
        &self,
        local_path: &str,
        progress: &(dyn Fn(u64, u64) + Send + Sync),
    ) -> Result<String, HkError> {
        let meta = tokio::fs::metadata(local_path)
            .await
            .map_err(|e| HkError::invalid(format!("{local_path}: {e}")))?;
        let total = meta.len();
        let name = std::path::Path::new(local_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("video")
            .to_owned();
        let file = tokio::fs::File::open(local_path)
            .await
            .map_err(|e| HkError::invalid(format!("{local_path}: {e}")))?;
        let (tx, mut rx) = mpsc::unbounded_channel::<u64>();
        let sent = std::sync::atomic::AtomicU64::new(0);
        let body_stream =
            tokio_util::io::ReaderStream::with_capacity(file, 256 * 1024).map(move |chunk| {
                if let Ok(c) = &chunk {
                    let n = sent.fetch_add(c.len() as u64, std::sync::atomic::Ordering::Relaxed)
                        + c.len() as u64;
                    let _ = tx.send(n);
                }
                chunk
            });
        let rb = self
            .i
            .request(Method::PUT, &["studio", "upload"])
            .query(&[("name", name.as_str())])
            .header(reqwest::header::CONTENT_LENGTH, total)
            .timeout(Duration::from_secs(12 * 3600))
            .body(reqwest::Body::wrap_stream(body_stream));
        let fut = self.i.json::<Value>(rb);
        tokio::pin!(fut);
        let reply = loop {
            tokio::select! {
                r = &mut fut => break r?,
                Some(n) = rx.recv() => progress(n, total),
            }
        };
        progress(total, total);
        reply
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| HkError::internal("instance did not return an upload path"))
    }

    async fn studio_download(&self, server_path: &str, dest: &str) -> Result<(), HkError> {
        use tokio::io::AsyncWriteExt;
        let rb = self
            .i
            .request(Method::GET, &["studio", "media"])
            .query(&[("path", server_path)])
            .timeout(Duration::from_secs(12 * 3600));
        let res = self.i.send(rb).await?;
        let part = format!("{dest}.part");
        let mut file = tokio::fs::File::create(&part)
            .await
            .map_err(|e| HkError::invalid(format!("{dest}: {e}")))?;
        let mut bytes = res.bytes_stream();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(net_err)?;
            file.write_all(&chunk)
                .await
                .map_err(|e| HkError::internal(e.to_string()))?;
        }
        file.flush()
            .await
            .map_err(|e| HkError::internal(e.to_string()))?;
        tokio::fs::rename(&part, dest)
            .await
            .map_err(|e| HkError::internal(e.to_string()))
    }

    async fn shutdown(&self) {
        self.i.shutdown.cancel();
    }
}

/// Test runs answer when finished (instance cap 120 s).
const RUN_TIMEOUT: Duration = Duration::from_secs(130);
/// Workflows scanned when looking for an earlier upload of the same app workflow.
const UPLOAD_SCAN_PAGES: usize = 4;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// n8n execution JSON -> list row. `name` / `error` / `nodes` are known only for detail reads.
fn execution_summary(
    e: &Value,
    names: &std::collections::HashMap<String, String>,
    error: Option<String>,
    nodes: Option<u32>,
) -> FlowRunSummaryDto {
    let ms = |k: &str| {
        e.get(k)
            .and_then(Value::as_str)
            .and_then(flow_runs::parse_iso)
    };
    let started = ms("startedAt").unwrap_or(0);
    let finished = ms("stoppedAt");
    let wid = e
        .get("workflowId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    FlowRunSummaryDto {
        id: e
            .get("id")
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
            .unwrap_or_default(),
        workflow_name: wid
            .as_ref()
            .and_then(|w| names.get(w).cloned())
            .unwrap_or_else(|| "Workflow".into()),
        workflow_id: wid,
        mode: e
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("manual")
            .to_owned(),
        status: e
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        started_ms: started,
        finished_ms: finished,
        duration_ms: finished.map_or(0, |f| (f - started).max(0)),
        error,
        node_count: nodes.unwrap_or(0),
        remote: true,
    }
}

impl Inner {
    /// Workflow id -> name for the list; failure just means generic names.
    async fn workflow_names(&self) -> std::collections::HashMap<String, String> {
        let page: Result<Value, _> = self
            .json(
                self.request(Method::GET, &["workflows"])
                    .query(&[("limit", "250")]),
            )
            .await;
        page.ok()
            .and_then(|p| p.get("data").cloned())
            .and_then(|d| d.as_array().cloned())
            .into_iter()
            .flatten()
            .filter_map(|w| {
                Some((
                    w.get("id")?.as_str()?.to_owned(),
                    w.get("name")?.as_str()?.to_owned(),
                ))
            })
            .collect()
    }

    /// The instance's copy of an app workflow, created / refreshed so a run sees the canvas as
    /// it is now. The copy is tagged `meta.hokkabaz.localId` (the app's workflow id), which is
    /// how a later session finds it again; a rename in the app updates it, not duplicates it.
    async fn ensure_workflow(&self, workflow_json: &str) -> Result<String, HkError> {
        let mut wf: Value = serde_json::from_str(workflow_json)
            .map_err(|e| HkError::invalid(format!("workflow JSON: {e}")))?;
        let obj = wf
            .as_object_mut()
            .ok_or_else(|| HkError::invalid("workflow must be a JSON object"))?;
        let local_id = obj
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| HkError::invalid("workflow has no id"))?
            .to_owned();
        let meta = obj.entry("meta").or_insert_with(|| json!({}));
        if !meta.is_object() {
            *meta = json!({});
        }
        if let Some(m) = meta.as_object_mut() {
            let h = m.entry("hokkabaz").or_insert_with(|| json!({}));
            if let Some(h) = h.as_object_mut() {
                h.insert("localId".into(), json!(local_id));
            }
        }

        let mut known = lock(&self.flow_ids).get(&local_id).cloned();
        let mut cursor: Option<String> = None;
        for _ in 0..UPLOAD_SCAN_PAGES {
            if known.is_some() {
                break;
            }
            let mut q = vec![("limit", "250".to_owned())];
            if let Some(c) = &cursor {
                q.push(("cursor", c.clone()));
            }
            let page: Value = self
                .json(self.request(Method::GET, &["workflows"]).query(&q))
                .await?;
            known = page
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|w| {
                    w.pointer("/meta/hokkabaz/localId").and_then(Value::as_str)
                        == Some(local_id.as_str())
                })
                .and_then(|w| w.get("id").and_then(Value::as_str))
                .map(str::to_owned);
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        let remote_id = match known {
            Some(rid) => {
                let put = self.request(Method::PUT, &["workflows", &rid]).json(&wf);
                match self.send(put).await {
                    Ok(_) => rid,
                    // Deleted on the instance since we cached it: create it again below.
                    Err(e) if e.kind == HkErrorKind::NotFound => self.create_workflow(&wf).await?,
                    Err(e) => return Err(e),
                }
            }
            None => self.create_workflow(&wf).await?,
        };
        lock(&self.flow_ids).insert(local_id, remote_id.clone());
        Ok(remote_id)
    }

    async fn create_workflow(&self, wf: &Value) -> Result<String, HkError> {
        let created: Value = self
            .json(self.request(Method::POST, &["workflows"]).json(wf))
            .await?;
        created
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| HkError::internal("instance created a workflow without an id"))
    }

    async fn run_detail(&self, id: &str) -> Result<FlowRunDetailDto, HkError> {
        let e: Value = self
            .json(
                self.request(Method::GET, &["executions", id])
                    .query(&[("includeData", "true")]),
            )
            .await?;
        let result = e.pointer("/data/resultData");
        let mut run_data = result
            .and_then(|r| r.get("runData"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        flow_runs::shrink_run_data(&mut run_data, DETAIL_MAX_ITEMS, DETAIL_MAX_BYTES);
        let error = result
            .and_then(|r| r.pointer("/error/message"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let nodes =
            u32::try_from(run_data.as_object().map_or(0, serde_json::Map::len)).unwrap_or(u32::MAX);
        let wf: Option<Value> = match e.get("workflowId").and_then(Value::as_str) {
            Some(w) => self
                .json(self.request(Method::GET, &["workflows", w]))
                .await
                .ok(),
            None => None,
        };
        let mut names = std::collections::HashMap::new();
        if let (Some(w), Some(n)) = (
            e.get("workflowId").and_then(Value::as_str),
            wf.as_ref()
                .and_then(|w| w.get("name"))
                .and_then(Value::as_str),
        ) {
            names.insert(w.to_owned(), n.to_owned());
        }
        Ok(FlowRunDetailDto {
            summary: execution_summary(&e, &names, error, Some(nodes)),
            run_data_json: run_data.to_string(),
            workflow_json: wf.map(|w| w.to_string()),
        })
    }
}

fn bad_reply(e: reqwest::Error) -> HkError {
    HkError::internal(format!("bad reply from instance: {}", e.without_url()))
}

// ------------------------------------------------------------------------- live feed

/// Resume state of one feed.
#[derive(Default)]
struct Cursor {
    /// Highest durable `seq` handed to the consumer.
    seen: Option<u64>,
    /// Last `seq` that existed when the feed first attached; replay never goes below it.
    floor: Option<u64>,
    baselined: bool,
}

impl Cursor {
    fn after(&self) -> Option<u64> {
        self.seen.max(self.floor)
    }
}

enum Ended {
    /// Unrecoverable (bad token, unknown session): reported once, feed ends.
    Fatal(String),
    /// Consumer dropped the stream or the core shut down.
    Closed,
    /// Socket lost; `live` = it had been subscribed.
    Dropped { live: bool },
}

async fn pump(
    i: Arc<Inner>,
    sid: String,
    tx: mpsc::Sender<SessionEventDto>,
    ready: watch::Sender<bool>,
) {
    let mut cur = Cursor::default();
    let mut backoff = BACKOFF_MIN;
    let mut announced = false;
    loop {
        let ended = tokio::select! {
            () = i.shutdown.cancelled() => return,
            () = tx.closed() => return,
            e = connection(&i, &sid, &tx, &mut cur, &ready) => e,
        };
        match ended {
            Ended::Closed => return,
            Ended::Fatal(message) => {
                let ev = SessionEventDto {
                    text: Some(message),
                    fatal: true,
                    ..SessionEventDto::new(SessionEventKind::Error)
                };
                let _ = tx.send(ev).await;
                return;
            }
            Ended::Dropped { live } => {
                if live {
                    backoff = BACKOFF_MIN;
                    announced = false;
                }
                if !announced {
                    announced = true;
                    let ev = SessionEventDto {
                        text: Some("connection to the instance lost; reconnecting".into()),
                        ..SessionEventDto::new(SessionEventKind::Error)
                    };
                    if tx.send(ev).await.is_err() {
                        return;
                    }
                }
            }
        }
        tokio::select! {
            () = i.shutdown.cancelled() => return,
            () = tx.closed() => return,
            () = tokio::time::sleep(jitter(backoff)) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// +0..25% so a fleet of apps does not reconnect in lockstep after a daemon restart.
fn jitter(d: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |t| t.subsec_nanos());
    d + d / 4 * (nanos % 1000) / 1000
}

/// One socket lifetime: connect, subscribe, catch up, then forward frames.
async fn connection(
    i: &Inner,
    sid: &str,
    tx: &mpsc::Sender<SessionEventDto>,
    cur: &mut Cursor,
    ready: &watch::Sender<bool>,
) -> Ended {
    let mut ws = match ws_connect(&i.base, &i.token).await {
        Ok(ws) => ws,
        Err(ConnectError::Auth(m)) => return Ended::Fatal(m),
        Err(ConnectError::Other(m)) => {
            tracing::debug!(error = %m, "instance socket connect failed");
            return Ended::Dropped { live: false };
        }
    };
    let sub = json!({ "cmd": "subscribe", "session_id": sid, "id": SUB_ID }).to_string();
    if ws.send(Message::text(sub)).await.is_err() {
        return Ended::Dropped { live: false };
    }
    let mut live = false;
    let mut last_rx = Instant::now();
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if last_rx.elapsed() > IDLE_LIMIT || ws.send(Message::Ping(Default::default())).await.is_err() {
                    return Ended::Dropped { live };
                }
            }
            msg = ws.next() => {
                last_rx = Instant::now();
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_)) | Err(_)) | None => return Ended::Dropped { live },
                    Some(Ok(_)) => continue,
                };
                let Ok(frame) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                let name = frame.get("event").and_then(Value::as_str).unwrap_or_default();
                let action = match name {
                    "command/ack" if frame.get("id").and_then(Value::as_str) == Some(SUB_ID) => {
                        live = true;
                        let r = sync(i, sid, tx, cur).await;
                        if r.is_ok() {
                            ready.send_replace(true);
                        }
                        r
                    }
                    "command/error" if frame.get("id").and_then(Value::as_str) == Some(SUB_ID) => {
                        let m = frame.pointer("/data/message").and_then(Value::as_str).unwrap_or("subscribe refused");
                        Err(Ended::Fatal(m.to_owned()))
                    }
                    "stream/lagged" => sync(i, sid, tx, cur).await,
                    _ if frame.get("session_id").and_then(Value::as_str) == Some(sid) => {
                        match serde_json::from_value::<ServerEvent>(frame) {
                            Ok(ev) => forward(tx, cur, ev).await,
                            Err(_) => Ok(()),
                        }
                    }
                    _ => Ok(()),
                };
                if let Err(end) = action {
                    return end;
                }
            }
        }
    }
}

/// Run `ev` through dedupe and the DTO mapping into the feed.
async fn forward(
    tx: &mpsc::Sender<SessionEventDto>,
    cur: &mut Cursor,
    ev: ServerEvent,
) -> Result<(), Ended> {
    if let Some(seq) = ev.data.get("seq").and_then(Value::as_u64) {
        // Already delivered by an earlier socket or by a replay.
        if cur.seen.is_some_and(|s| seq <= s) {
            return Ok(());
        }
        cur.seen = Some(seq);
    }
    for dto in events::map(ev) {
        tx.send(dto).await.map_err(|_| Ended::Closed)?;
    }
    Ok(())
}

/// First subscribe: remember where the log stood (the feed carries only what happens after
/// attaching). Later ones: replay what the previous socket missed.
async fn sync(
    i: &Inner,
    sid: &str,
    tx: &mpsc::Sender<SessionEventDto>,
    cur: &mut Cursor,
) -> Result<(), Ended> {
    let fail = |e: HkError| match e.kind {
        HkErrorKind::NotFound | HkErrorKind::Unauthorized => Ended::Fatal(e.message),
        _ => Ended::Dropped { live: true },
    };
    if !cur.baselined {
        // A cursor past the end returns no events but still carries `event_count`.
        let d = i.detail(sid, Some(u64::MAX)).await.map_err(fail)?;
        cur.floor = d.session.event_count.checked_sub(1);
        cur.baselined = true;
        return Ok(());
    }
    let d = i.detail(sid, cur.after()).await.map_err(fail)?;
    let mut replay: VecDeque<Value> = d.events.into();
    while let Some(data) = replay.pop_front() {
        let name = data
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("session/event")
            .to_owned();
        forward(
            tx,
            cur,
            ServerEvent {
                name,
                session_id: sid.to_owned(),
                data,
            },
        )
        .await?;
    }
    Ok(())
}

// ----------------------------------------------------------------------- websocket

enum ConnectError {
    /// 401/403 on upgrade: retrying cannot help.
    Auth(String),
    Other(String),
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type Ws = tokio_tungstenite::WebSocketStream<Box<dyn Io>>;

fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CFG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CFG.get_or_init(|| {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map(|b| Arc::new(b.with_root_certificates(roots).with_no_client_auth()))
            .map_err(|e| format!("tls config: {e}"))
    })
    .clone()
}

async fn ws_connect(base: &Url, token: &Secret) -> Result<Ws, ConnectError> {
    let other = |m: String| ConnectError::Other(m);
    let tls = base.scheme() == "https";
    let mut url = base.clone();
    if let Ok(mut s) = url.path_segments_mut() {
        s.pop_if_empty().extend(["api", "v1", "ws"]);
    }
    url.set_scheme(if tls { "wss" } else { "ws" })
        .map_err(|()| other("cannot derive websocket URL".into()))?;
    let host = base
        .host_str()
        .ok_or_else(|| other("URL has no host".into()))?
        .to_owned();
    let port = base
        .port_or_known_default()
        .ok_or_else(|| other("URL has no port".into()))?;

    let mut req = url
        .as_str()
        .into_client_request()
        .map_err(|e| other(e.to_string()))?;
    let mut auth = HeaderValue::from_str(&format!("Bearer {}", token.expose())).map_err(|_| {
        ConnectError::Auth("token contains characters a header cannot carry".into())
    })?;
    auth.set_sensitive(true);
    req.headers_mut()
        .insert(tungstenite::http::header::AUTHORIZATION, auth);

    let handshake = async {
        let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| other(e.to_string()))?;
        // Small frames (deltas) must not wait for Nagle.
        let _ = tcp.set_nodelay(true);
        let io: Box<dyn Io> = if tls {
            let name = rustls::pki_types::ServerName::try_from(host.clone())
                .map_err(|e| other(e.to_string()))?;
            let cfg = tls_config().map_err(other)?;
            let s = tokio_rustls::TlsConnector::from(cfg)
                .connect(name, tcp)
                .await
                .map_err(|e| other(e.to_string()))?;
            Box::new(s)
        } else {
            Box::new(tcp)
        };
        match tokio_tungstenite::client_async(req, io).await {
            Ok((ws, _)) => Ok(ws),
            Err(tungstenite::Error::Http(res)) if matches!(res.status().as_u16(), 401 | 403) => {
                Err(ConnectError::Auth(format!(
                    "instance refused the token ({})",
                    res.status()
                )))
            }
            Err(e) => Err(other(e.to_string())),
        }
    };
    tokio::time::timeout(CONNECT_TIMEOUT, handshake)
        .await
        .map_err(|_| other("websocket connect timed out".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_normalises() {
        assert_eq!(
            parse_base_url(" http://h:7878/ ").unwrap().as_str(),
            "http://h:7878/"
        );
        let u = parse_base_url("https://box.example/hk/?x=1#f").unwrap();
        assert_eq!(u.path(), "/hk");
        assert!(parse_base_url("ftp://h").is_err());
        assert!(parse_base_url("http://u:p@h").is_err());
        assert!(parse_base_url("nope").is_err());
    }

    #[test]
    fn api_url_keeps_prefix_and_encodes() {
        let core = RemoteCore::new(
            "https://box.example/hk",
            Secret::new("t"),
            CoreProfile::Desktop,
        )
        .unwrap();
        assert_eq!(
            core.i.url(&["sessions", "a b"]).as_str(),
            "https://box.example/hk/api/v1/sessions/a%20b"
        );
        let core = RemoteCore::new(
            "http://127.0.0.1:1/",
            Secret::new("t"),
            CoreProfile::Desktop,
        )
        .unwrap();
        assert_eq!(
            core.i.url(&["health"]).as_str(),
            "http://127.0.0.1:1/api/v1/health"
        );
    }

    #[test]
    fn cursor_replays_from_the_later_of_seen_and_floor() {
        let mut c = Cursor::default();
        assert_eq!(c.after(), None);
        c.floor = Some(5);
        assert_eq!(c.after(), Some(5));
        c.seen = Some(9);
        assert_eq!(c.after(), Some(9));
    }
}
