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
use hk_server::{ApprovalDecision, CreateSession, ServerEvent, SessionInfo, TurnAccepted, TurnMode, TurnRequest};
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
    CoreProfile, CreateSessionDto, InstanceStateDto, InstanceStatusDto, ModeDto, ModelListDto, PreferencesDto,
    ProviderInputDto, SessionDetailDto, SessionEventDto, SessionEventKind, SessionInfoDto, SettingsDto,
    TurnAcceptedDto, TurnRequestDto,
};
use crate::error::{HkError, HkErrorKind};
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

struct Inner {
    base: Url,
    token: Secret,
    http: reqwest::Client,
    profile: CoreProfile,
    shutdown: CancellationToken,
    /// Feeds still attaching, per session: commands wait for them so a turn submitted right after
    /// `session_events` cannot start before the socket listens.
    feeds: Mutex<Vec<(String, watch::Receiver<bool>)>>,
}

/// Remote [`CoreHandle`]. Cheap to share via `Arc`.
pub struct RemoteCore {
    i: Arc<Inner>,
}

impl std::fmt::Debug for RemoteCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteCore").field("base", &self.i.base.as_str()).finish_non_exhaustive()
    }
}

/// Validate and normalise an instance URL: http(s) only, no query/fragment, no trailing slash.
pub fn parse_base_url(raw: &str) -> Result<Url, HkError> {
    let mut url = Url::parse(raw.trim()).map_err(|e| HkError::invalid(format!("invalid URL: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(HkError::invalid("URL must start with http:// or https://"));
    }
    if url.host_str().is_none() {
        return Err(HkError::invalid("URL has no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(HkError::invalid("URL must not carry credentials; use the token field"));
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
    let kind = if e.is_connect() || e.is_timeout() { HkErrorKind::Unavailable } else { HkErrorKind::Internal };
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

fn unsupported(what: &str) -> HkError {
    HkError::new(HkErrorKind::Unsupported, format!("{what} is managed on the instance, not from this app"))
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
        self.http.request(method, self.url(segments)).bearer_auth(self.token.expose())
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::Response, HkError> {
        check(rb.send().await.map_err(net_err)?).await
    }

    async fn json<T: DeserializeOwned>(&self, rb: reqwest::RequestBuilder) -> Result<T, HkError> {
        self.send(rb).await?.json().await.map_err(|e| HkError::internal(format!("bad reply from instance: {}", e.without_url())))
    }

    async fn post_json(&self, segments: &[&str], body: &impl Serialize) -> Result<reqwest::Response, HkError> {
        self.send(self.request(Method::POST, segments).json(body)).await
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
            feeds.iter().filter(|(s, r)| s == sid && !*r.borrow()).map(|(_, r)| r.clone()).collect()
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
    let (base, http) = match parse_base_url(base_url).and_then(|b| Ok((b, http_client(PROBE_TIMEOUT)?))) {
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
    };
    let started = Instant::now();
    let health: Result<Value, HkError> = inner.json(inner.http.get(inner.url(&["health"]))).await;
    let latency = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let health = match health {
        Ok(h) => h,
        Err(e) => return status(InstanceStateDto::Unreachable, None, None, Some(e.message)),
    };
    let version = health.get("version").and_then(Value::as_str).map(str::to_owned);
    if token.is_none() {
        return status(InstanceStateDto::Unauthorized, version, Some(latency), Some("no token stored".into()));
    }
    match inner.send(inner.request(Method::GET, &["sessions"]).query(&[("limit", 1)])).await {
        Ok(_) => status(InstanceStateDto::Online, version, Some(latency), None),
        Err(e) if e.kind == HkErrorKind::Unauthorized => {
            status(InstanceStateDto::Unauthorized, version, Some(latency), Some(e.message))
        }
        Err(e) => status(InstanceStateDto::Unreachable, version, Some(latency), Some(e.message)),
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
    };
    let rb = inner.http.post(inner.url(&["pairing", "exchange"])).json(&json!({ "token": pairing_token }));
    match inner.json::<Exchanged>(rb).await {
        Ok(x) => Ok(Secret::new(x.token)),
        // The server answers every bad token the same way; say what the user can act on.
        Err(e) if e.kind == HkErrorKind::Unauthorized => {
            Err(HkError::new(HkErrorKind::Unauthorized, "pairing link is invalid, expired or already used"))
        }
        Err(e) if e.kind == HkErrorKind::NotFound => {
            Err(HkError::new(HkErrorKind::Unsupported, "this instance has pairing disabled"))
        }
        Err(e) => Err(e),
    }
}

#[async_trait]
impl CoreHandle for RemoteCore {
    fn profile(&self) -> CoreProfile {
        self.i.profile
    }

    async fn get_settings(&self) -> Result<SettingsDto, HkError> {
        Err(unsupported("Provider setup"))
    }

    async fn save_provider(&self, _input: ProviderInputDto) -> Result<SettingsDto, HkError> {
        Err(unsupported("Provider setup"))
    }

    async fn remove_provider(&self, _id: String) -> Result<SettingsDto, HkError> {
        Err(unsupported("Provider setup"))
    }

    async fn set_api_key(&self, _provider_id: String, _key: Option<String>) -> Result<SettingsDto, HkError> {
        Err(unsupported("API keys"))
    }

    async fn set_preferences(&self, _prefs: PreferencesDto) -> Result<SettingsDto, HkError> {
        Err(unsupported("Preferences"))
    }

    async fn list_models(&self) -> Result<ModelListDto, HkError> {
        #[derive(serde::Deserialize)]
        struct Models {
            models: Vec<ModelInfo>,
        }
        let m: Models = self.i.json(self.i.request(Method::GET, &["models"])).await?;
        Ok(ModelListDto { models: m.models.iter().map(settings::model_dto).collect(), errors: Vec::new() })
    }

    async fn refresh_catalog(&self) -> Result<(), HkError> {
        Err(unsupported("The model catalog"))
    }

    async fn create_session(&self, req: CreateSessionDto) -> Result<SessionInfoDto, HkError> {
        let body = CreateSession { cwd: req.workspace_root, model: req.model, title: req.title };
        let info: SessionInfo = self.i.post_json(&["sessions"], &body).await?.json().await.map_err(bad_reply)?;
        Ok(session_dto(info))
    }

    async fn list_sessions(&self, limit: u32, offset: u32) -> Result<Vec<SessionInfoDto>, HkError> {
        #[derive(serde::Deserialize)]
        struct Page {
            sessions: Vec<SessionInfo>,
        }
        let rb = self.i.request(Method::GET, &["sessions"]).query(&[("limit", limit), ("offset", offset)]);
        let p: Page = self.i.json(rb).await?;
        Ok(p.sessions.into_iter().map(session_dto).collect())
    }

    async fn get_session(&self, session_id: &str) -> Result<SessionDetailDto, HkError> {
        let d = self.i.detail(session_id, None).await?;
        let logged: Vec<hk_session::SessionEvent> =
            d.events.into_iter().filter_map(|e| serde_json::from_value(e).ok()).collect();
        Ok(SessionDetailDto {
            session: session_dto(d.session),
            messages: d.messages.into_iter().map(message_dto).collect(),
            pending_approvals: pending_approvals(&logged, &d.pending_approvals),
        })
    }

    async fn submit_turn(&self, session_id: &str, req: TurnRequestDto) -> Result<TurnAcceptedDto, HkError> {
        if req.prompt.trim().is_empty() {
            return Err(HkError::invalid("prompt must not be empty"));
        }
        self.i.feeds_ready(session_id).await;
        let body = TurnRequest {
            prompt: req.prompt,
            mode: req.mode.map(|m| match m {
                ModeDto::Build => TurnMode::Build,
                ModeDto::Plan => TurnMode::Plan,
            }),
            model: req.model,
        };
        let a: TurnAccepted =
            self.i.post_json(&["sessions", session_id, "turns"], &body).await?.json().await.map_err(bad_reply)?;
        Ok(TurnAcceptedDto { session_id: a.session_id, queued: a.queued })
    }

    async fn inject(&self, session_id: &str, text: String) -> Result<(), HkError> {
        self.i.feeds_ready(session_id).await;
        self.i.post_json(&["sessions", session_id, "inject"], &json!({ "text": text })).await.map(drop)
    }

    async fn cancel(&self, session_id: &str) -> Result<(), HkError> {
        self.i.send(self.i.request(Method::POST, &["sessions", session_id, "cancel"])).await.map(drop)
    }

    async fn respond_approval(
        &self,
        session_id: &str,
        approval_id: &str,
        approved: bool,
        reason: Option<String>,
    ) -> Result<(), HkError> {
        let body = ApprovalDecision { approved, reason, session_id: Some(session_id.to_owned()) };
        self.i.post_json(&["approvals", approval_id], &body).await.map(drop)
    }

    fn session_events(&self, session_id: &str) -> Result<BoxStream<'static, SessionEventDto>, HkError> {
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

    async fn shutdown(&self) {
        self.i.shutdown.cancel();
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

async fn pump(i: Arc<Inner>, sid: String, tx: mpsc::Sender<SessionEventDto>, ready: watch::Sender<bool>) {
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
async fn forward(tx: &mpsc::Sender<SessionEventDto>, cur: &mut Cursor, ev: ServerEvent) -> Result<(), Ended> {
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
async fn sync(i: &Inner, sid: &str, tx: &mpsc::Sender<SessionEventDto>, cur: &mut Cursor) -> Result<(), Ended> {
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
        let name = data.get("type").and_then(Value::as_str).unwrap_or("session/event").to_owned();
        forward(tx, cur, ServerEvent { name, session_id: sid.to_owned(), data }).await?;
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
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
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
    url.set_scheme(if tls { "wss" } else { "ws" }).map_err(|()| other("cannot derive websocket URL".into()))?;
    let host = base.host_str().ok_or_else(|| other("URL has no host".into()))?.to_owned();
    let port = base.port_or_known_default().ok_or_else(|| other("URL has no port".into()))?;

    let mut req = url.as_str().into_client_request().map_err(|e| other(e.to_string()))?;
    let mut auth = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
        .map_err(|_| ConnectError::Auth("token contains characters a header cannot carry".into()))?;
    auth.set_sensitive(true);
    req.headers_mut().insert(tungstenite::http::header::AUTHORIZATION, auth);

    let handshake = async {
        let tcp = tokio::net::TcpStream::connect((host.as_str(), port)).await.map_err(|e| other(e.to_string()))?;
        // Small frames (deltas) must not wait for Nagle.
        let _ = tcp.set_nodelay(true);
        let io: Box<dyn Io> = if tls {
            let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| other(e.to_string()))?;
            let cfg = tls_config().map_err(other)?;
            let s = tokio_rustls::TlsConnector::from(cfg).connect(name, tcp).await.map_err(|e| other(e.to_string()))?;
            Box::new(s)
        } else {
            Box::new(tcp)
        };
        match tokio_tungstenite::client_async(req, io).await {
            Ok((ws, _)) => Ok(ws),
            Err(tungstenite::Error::Http(res)) if matches!(res.status().as_u16(), 401 | 403) => {
                Err(ConnectError::Auth(format!("instance refused the token ({})", res.status())))
            }
            Err(e) => Err(other(e.to_string())),
        }
    };
    tokio::time::timeout(CONNECT_TIMEOUT, handshake).await.map_err(|_| other("websocket connect timed out".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_normalises() {
        assert_eq!(parse_base_url(" http://h:7878/ ").unwrap().as_str(), "http://h:7878/");
        let u = parse_base_url("https://box.example/hk/?x=1#f").unwrap();
        assert_eq!(u.path(), "/hk");
        assert!(parse_base_url("ftp://h").is_err());
        assert!(parse_base_url("http://u:p@h").is_err());
        assert!(parse_base_url("nope").is_err());
    }

    #[test]
    fn api_url_keeps_prefix_and_encodes() {
        let core = RemoteCore::new("https://box.example/hk", Secret::new("t"), CoreProfile::Desktop).unwrap();
        assert_eq!(core.i.url(&["sessions", "a b"]).as_str(), "https://box.example/hk/api/v1/sessions/a%20b");
        let core = RemoteCore::new("http://127.0.0.1:1/", Secret::new("t"), CoreProfile::Desktop).unwrap();
        assert_eq!(core.i.url(&["health"]).as_str(), "http://127.0.0.1:1/api/v1/health");
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
