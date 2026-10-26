//! Model router: per-agent model chains, retry/backoff, fallback, token meter
//! and cost projection. [`Router`] implements [`Provider`], so the agent loop
//! consumes it exactly like a single adapter.
// port: dsh packages/llm `llm-retry` (backoff), opencode packages/opencode/src/provider (model refs)
//!
//! # Addressing
//!
//! `ChatRequest.model` is a route key, resolved in this order:
//! 1. a chain registered with [`Router::chain`] under that exact key (typically
//!    an agent or mode name, e.g. `build`, or a full ref like `deepseek/deepseek-chat`);
//! 2. a direct `provider/model` ref (single hop, no fallback; the model part
//!    may itself contain `/`, e.g. `openrouter/anthropic/claude-sonnet`);
//! 3. the default chain, if one was set with [`Router::default_chain`].
//!
//! # Retry and fallback policy
//!
//! Failover only happens *before the first stream event* has been seen. The
//! router waits for that first event (bounded by `first_event_timeout`) so a
//! 5xx, timeout or dropped connection on a route moves on to the next one
//! without the caller observing anything. Once an event has been handed out
//! the answer is committed: later errors surface unchanged, because replaying
//! half a response on another model would duplicate or contradict visible
//! text and tool calls. Recovering from that is the agent's call (it logs
//! `assistant/attempt`).
//!
//! Per error, before the first event:
//! - `Cancelled`: stop immediately, no retry, no fallback. The caller asked.
//! - retryable (`Network`, HTTP 408/409/425/429/5xx, first-event timeout):
//!   retry the same route with backoff up to `RetryPolicy::max_retries`
//!   (honouring `Retry-After`), then fall back to the next route.
//! - everything else (`Auth`, `ContextLength`, `InvalidRequest`, `Protocol`,
//!   other 4xx): never retried on the same route (it would fail identically),
//!   but the next route is still tried, since credentials, context window and
//!   feature support are per-provider/model. If every route fails, the error
//!   of the last route tried is returned and each hop is logged via `tracing`.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures::StreamExt;
use futures::stream;

use crate::catalog::Catalog;
use crate::openai::RetryPolicy;
use crate::{
    CancellationToken, ChatRequest, Cost, EventStream, LlmError, ModelInfo, Provider, Result,
    StreamEvent, Usage,
};

/// A concrete `provider/model` target.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Route {
    /// Provider id registered with the router.
    pub provider: String,
    /// Provider-local model id (may contain `/`).
    pub model: String,
}

impl Route {
    /// Parse `provider/model`, splitting at the first `/`.
    pub fn parse(s: &str) -> Result<Self> {
        match s.split_once('/') {
            Some((p, m)) if !p.is_empty() && !m.is_empty() => Ok(Self {
                provider: p.to_owned(),
                model: m.to_owned(),
            }),
            _ => Err(LlmError::InvalidRequest(format!(
                "expected `provider/model`, got {s:?}"
            ))),
        }
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

/// Usage and cost accumulated for one route.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RouteTotals {
    /// Requests that produced at least one event.
    pub requests: u64,
    /// Summed token usage.
    pub usage: Usage,
    /// Summed cost; unpriced models contribute 0.
    pub cost: Cost,
}

/// Snapshot of the token meter.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meter {
    /// Usage across all routes.
    pub usage: Usage,
    /// Cost across all routes.
    pub cost: Cost,
    /// Requests whose model had no price in the catalog (cost counted as 0).
    pub unpriced_requests: u64,
    /// Breakdown keyed by `provider/model`.
    pub by_route: HashMap<String, RouteTotals>,
}

/// Cost estimate for a request before sending it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projection {
    /// Estimated prompt tokens (chars / 4 heuristic; not a tokenizer).
    pub input_tokens: u64,
    /// Output tokens the caller expects.
    pub output_tokens: u64,
    /// Estimated cost on the primary route.
    pub cost: Cost,
}

/// Provider that dispatches to other providers. See module docs.
#[derive(Clone)]
pub struct Router {
    providers: HashMap<String, Arc<dyn Provider>>,
    chains: HashMap<String, Vec<Route>>,
    default: Option<Vec<Route>>,
    retry: RetryPolicy,
    first_event_timeout: Option<Duration>,
    catalog: Option<Arc<Catalog>>,
    meter: Arc<Mutex<Meter>>,
    budget: Option<Budget>,
    auto: Option<AutoRoutes>,
}

/// Spend cap. Once reached, priced routes are skipped (unpriced/local ones still run);
/// with `hard_stop` and no unpriced route left the request fails.
#[derive(Clone)]
pub struct Budget {
    pub limit_usd: f64,
    pub hard_stop: bool,
    /// Spend from outside this router (e.g. today's persisted usage); added to the meter.
    pub external_spend: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
}

impl fmt::Debug for Budget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Budget")
            .field("limit_usd", &self.limit_usd)
            .field("hard_stop", &self.hard_stop)
            .finish_non_exhaustive()
    }
}

/// Task kind picked for model `auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskKind {
    Code,
    LongContext,
    Reasoning,
    Cheap,
}

/// Chain keys per task kind for requests with model `auto`.
#[derive(Debug, Clone, Default)]
pub struct AutoRoutes {
    pub chains: HashMap<TaskKind, String>,
    /// Estimated prompt tokens above which a request counts as long-context.
    pub long_context_tokens: u64,
}

impl AutoRoutes {
    /// Heuristic task classification from the request text.
    pub fn classify(&self, req: &ChatRequest) -> TaskKind {
        let chars: usize = req.messages.iter().map(|m| m.content.len()).sum();
        if self.long_context_tokens > 0 && (chars / 4) as u64 > self.long_context_tokens {
            return TaskKind::LongContext;
        }
        let last = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == crate::Role::User);
        let text = last.map_or("", |m| m.content.as_str()).to_lowercase();
        const CODE: [&str; 8] = [
            "```",
            "fn ",
            "def ",
            "class ",
            "error[",
            "traceback",
            "stack trace",
            "compile",
        ];
        const THINK: [&str; 5] = ["prove", "step by step", "why does", "plan ", "analyze"];
        if !req.tools.is_empty() || CODE.iter().any(|k| text.contains(k)) {
            TaskKind::Code
        } else if THINK.iter().any(|k| text.contains(k)) {
            TaskKind::Reasoning
        } else {
            TaskKind::Cheap
        }
    }
}

impl fmt::Debug for Router {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Router")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .field("chains", &self.chains)
            .field("default", &self.default)
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    /// Empty router. Retry defaults to one router-level retry per route:
    /// adapters already retry the connect phase themselves, so a larger
    /// budget here multiplies wait time before a fallback kicks in.
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            chains: HashMap::new(),
            default: None,
            retry: RetryPolicy {
                max_retries: 1,
                ..RetryPolicy::default()
            },
            first_event_timeout: Some(Duration::from_secs(60)),
            catalog: None,
            meter: Arc::default(),
            budget: None,
            auto: None,
        }
    }

    /// Register a provider under its [`Provider::id`].
    #[must_use]
    pub fn provider(mut self, p: Arc<dyn Provider>) -> Self {
        self.providers.insert(p.id().to_owned(), p);
        self
    }

    /// Register a fallback chain (first = primary) under `key`, e.g. an agent
    /// name. Providers must be registered first.
    pub fn chain<S: AsRef<str>>(mut self, key: &str, refs: &[S]) -> Result<Self> {
        let chain = self.parse_chain(refs)?;
        self.chains.insert(key.to_owned(), chain);
        Ok(self)
    }

    /// Chain used when `ChatRequest.model` is neither a registered key nor a
    /// `provider/model` ref.
    pub fn default_chain<S: AsRef<str>>(mut self, refs: &[S]) -> Result<Self> {
        self.default = Some(self.parse_chain(refs)?);
        Ok(self)
    }

    /// Per-route retry/backoff policy.
    #[must_use]
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    /// Max wait for a route to deliver its first event (`None` = unbounded).
    /// Expiry counts as a retryable network failure.
    #[must_use]
    pub fn first_event_timeout(mut self, t: Option<Duration>) -> Self {
        self.first_event_timeout = t;
        self
    }

    /// Catalog used for pricing, projection and capability checks.
    #[must_use]
    pub fn catalog(mut self, c: Arc<Catalog>) -> Self {
        self.catalog = Some(c);
        self
    }

    /// Same providers/chains/catalog with a zeroed meter: one router per session.
    #[must_use]
    pub fn with_fresh_meter(&self) -> Self {
        Self {
            meter: Arc::default(),
            ..self.clone()
        }
    }

    /// Copy of the current meter.
    /// Cap spend; see [`Budget`].
    pub fn budget(mut self, b: Budget) -> Self {
        self.budget = Some(b);
        self
    }

    /// Enable model `auto` routing by task kind.
    pub fn auto_routes(mut self, a: AutoRoutes) -> Self {
        self.auto = Some(a);
        self
    }

    fn over_budget(&self) -> bool {
        self.budget.as_ref().is_some_and(|b| {
            let spent = self.meter().cost.usd + b.external_spend.as_ref().map_or(0.0, |f| f());
            spent >= b.limit_usd
        })
    }

    pub fn meter(&self) -> Meter {
        self.meter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Estimate the cost of `req` on its primary route, assuming
    /// `expected_output_tokens` of output. `None` when the model is unpriced
    /// or unknown to the catalog.
    pub fn project(&self, req: &ChatRequest, expected_output_tokens: u64) -> Option<Projection> {
        let route = self.resolve(&req.model).ok()?.into_iter().next()?;
        let cost = self.cost_of(&route)?;
        let input_tokens = estimate_input_tokens(req);
        let usage = Usage {
            input_tokens,
            output_tokens: expected_output_tokens,
            ..Usage::default()
        };
        Some(Projection {
            input_tokens,
            output_tokens: expected_output_tokens,
            cost: cost.price(&usage),
        })
    }

    fn parse_chain<S: AsRef<str>>(&self, refs: &[S]) -> Result<Vec<Route>> {
        if refs.is_empty() {
            return Err(LlmError::InvalidRequest("empty fallback chain".into()));
        }
        refs.iter()
            .map(|r| {
                let route = Route::parse(r.as_ref())?;
                if self.providers.contains_key(&route.provider) {
                    Ok(route)
                } else {
                    Err(LlmError::InvalidRequest(format!(
                        "unknown provider {:?} in chain",
                        route.provider
                    )))
                }
            })
            .collect()
    }

    fn resolve(&self, key: &str) -> Result<Vec<Route>> {
        if let Some(c) = self.chains.get(key) {
            return Ok(c.clone());
        }
        if let Ok(r) = Route::parse(key)
            && self.providers.contains_key(&r.provider)
        {
            return Ok(vec![r]);
        }
        self.default
            .clone()
            .ok_or_else(|| LlmError::InvalidRequest(format!("no route for model {key:?}")))
    }

    fn model_info(&self, r: &Route) -> Option<&ModelInfo> {
        self.catalog.as_ref()?.model(&r.provider, &r.model)
    }

    fn cost_of(&self, r: &Route) -> Option<crate::ModelCost> {
        self.model_info(r)?.cost
    }

    /// Open one route and wait for its first event, so failures up to that
    /// point are still eligible for retry/fallback.
    async fn open(
        &self,
        route: &Route,
        req: ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<(StreamEvent, EventStream)> {
        let provider = &self.providers[&route.provider];
        let start = async {
            let mut s = provider.stream(req, cancel.clone()).await?;
            match s.next().await {
                Some(Ok(ev)) => Ok((ev, s)),
                Some(Err(e)) => Err(e),
                None => Err(LlmError::Protocol("stream ended before first event".into())),
            }
        };
        let bounded = async {
            match self.first_event_timeout {
                Some(d) => tokio::time::timeout(d, start).await.unwrap_or_else(|_| {
                    Err(LlmError::Network(
                        "timed out waiting for first event".into(),
                    ))
                }),
                None => start.await,
            }
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(LlmError::Cancelled),
            r = bounded => r,
        }
    }

    /// Wrap the committed stream so its final usage lands in the meter.
    fn metered(&self, route: &Route, first: StreamEvent, rest: EventStream) -> EventStream {
        let mut guard = MeterGuard {
            meter: Arc::clone(&self.meter),
            key: route.to_string(),
            price: self.cost_of(route),
            last: None,
        };
        guard.observe(&first);
        stream::once(async move { Ok(first) })
            .chain(rest.inspect(move |item| {
                if let Ok(ev) = item {
                    guard.observe(ev);
                }
            }))
            .boxed()
    }
}

/// Records the last reported usage when the stream ends or is dropped, so
/// partially consumed (errored/cancelled) requests are still billed.
struct MeterGuard {
    meter: Arc<Mutex<Meter>>,
    key: String,
    price: Option<crate::ModelCost>,
    last: Option<Usage>,
}

impl MeterGuard {
    fn observe(&mut self, ev: &StreamEvent) {
        if let StreamEvent::Usage(u) = ev {
            self.last = Some(*u); // last value wins
        }
    }
}

impl Drop for MeterGuard {
    fn drop(&mut self) {
        let usage = self.last.unwrap_or_default();
        let cost = self.price.map(|p| p.price(&usage)).unwrap_or_default();
        let mut m = self.meter.lock().unwrap_or_else(PoisonError::into_inner);
        m.usage.accumulate(&usage);
        m.cost.accumulate(&cost);
        if self.price.is_none() {
            m.unpriced_requests += 1;
        }
        let t = m.by_route.entry(std::mem::take(&mut self.key)).or_default();
        t.requests += 1;
        t.usage.accumulate(&usage);
        t.cost.accumulate(&cost);
    }
}

/// Rough prompt size: ~4 chars per token over everything the model sees.
fn estimate_input_tokens(req: &ChatRequest) -> u64 {
    let mut chars = 0usize;
    for m in &req.messages {
        chars += m.content.len() + m.reasoning.as_ref().map_or(0, String::len);
        for c in &m.tool_calls {
            chars += c.name.len() + c.arguments.to_string().len();
        }
    }
    for t in &req.tools {
        chars += t.name.len() + t.description.len() + t.parameters.to_string().len();
    }
    (chars as u64).div_ceil(4)
}

#[async_trait::async_trait]
impl Provider for Router {
    fn id(&self) -> &str {
        "router"
    }

    /// Union of all providers' models; a provider that errors is skipped.
    async fn models(&self) -> Result<Vec<ModelInfo>> {
        let mut ids: Vec<_> = self.providers.keys().cloned().collect();
        ids.sort();
        let mut out = Vec::new();
        for id in ids {
            match self.providers[&id].models().await {
                Ok(m) => out.extend(m),
                Err(e) => tracing::warn!(provider = %id, error = %e, "router: models() failed"),
            }
        }
        Ok(out)
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream> {
        let key = match (&self.auto, req.model.as_str()) {
            (Some(a), "auto") => {
                let kind = a.classify(&req);
                tracing::debug!(?kind, "router: auto route");
                a.chains
                    .get(&kind)
                    .cloned()
                    .unwrap_or_else(|| req.model.clone())
            }
            _ => req.model.clone(),
        };
        let chain = self.resolve(&key)?;
        let over = self.over_budget();
        let mut last_err = None;
        for route in &chain {
            if over
                && self
                    .cost_of(route)
                    .is_some_and(|c| c.input > 0.0 || c.output > 0.0)
            {
                tracing::info!(%route, "router: budget reached, skipping priced route");
                last_err = Some(LlmError::InvalidRequest("budget limit reached".into()));
                continue;
            }
            if !req.tools.is_empty()
                && self
                    .model_info(route)
                    .is_some_and(|m| !m.capabilities.tools)
            {
                tracing::debug!(%route, "router: skipping route without tool support");
                last_err = Some(LlmError::InvalidRequest(format!(
                    "{route} does not support tools"
                )));
                continue;
            }
            let mut retries = 0;
            loop {
                let mut attempt = req.clone();
                attempt.model.clone_from(&route.model);
                match self.open(route, attempt, &cancel).await {
                    Ok((first, rest)) => {
                        tracing::debug!(%route, retries, "router: route committed");
                        return Ok(self.metered(route, first, rest));
                    }
                    Err(LlmError::Cancelled) => return Err(LlmError::Cancelled),
                    Err(e) if e.is_retryable() && retries < self.retry.max_retries => {
                        let wait = self.retry.delay(retries, retry_after(&e));
                        tracing::warn!(%route, error = %e, ?wait, "router: retrying route");
                        tokio::select! {
                            biased;
                            () = cancel.cancelled() => return Err(LlmError::Cancelled),
                            () = tokio::time::sleep(wait) => {}
                        }
                        retries += 1;
                    }
                    Err(e) => {
                        tracing::warn!(%route, error = %e, "router: route failed, trying next");
                        last_err = Some(e);
                        break;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| LlmError::InvalidRequest("empty fallback chain".into())))
    }
}

fn retry_after(e: &LlmError) -> Option<Duration> {
    match e {
        LlmError::Http { retry_after, .. } => *retry_after,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::catalog::{CustomEndpoint, CustomModel};
    use crate::{Capabilities, FinishReason, Message, ModelCost, Role};

    /// What one scripted `stream()` call does.
    enum Step {
        /// `stream()` itself fails.
        Fail(fn() -> LlmError),
        /// Stream yields these items.
        Items(Vec<std::result::Result<StreamEvent, fn() -> LlmError>>),
        /// Never completes.
        Hang,
    }

    struct Mock {
        id: &'static str,
        script: Mutex<VecDeque<Step>>,
        calls: AtomicUsize,
        models_seen: Mutex<Vec<String>>,
    }

    impl Mock {
        fn new(id: &'static str, steps: Vec<Step>) -> Arc<Self> {
            Arc::new(Self {
                id,
                script: Mutex::new(steps.into()),
                calls: AtomicUsize::new(0),
                models_seen: Mutex::default(),
            })
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl Provider for Mock {
        fn id(&self) -> &str {
            self.id
        }
        async fn models(&self) -> Result<Vec<ModelInfo>> {
            Ok(vec![])
        }
        async fn stream(&self, req: ChatRequest, _c: CancellationToken) -> Result<EventStream> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.models_seen.lock().unwrap().push(req.model);
            let step = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .expect("script empty");
            match step {
                Step::Fail(f) => Err(f()),
                Step::Hang => std::future::pending().await,
                Step::Items(items) => Ok(stream::iter(
                    items
                        .into_iter()
                        .map(|i| i.map_err(|f| f()))
                        .collect::<Vec<_>>(),
                )
                .boxed()),
            }
        }
    }

    fn e500() -> LlmError {
        LlmError::Http {
            status: 503,
            message: "overloaded".into(),
            retry_after: None,
        }
    }
    fn e401() -> LlmError {
        LlmError::Auth("bad key".into())
    }
    fn enet() -> LlmError {
        LlmError::Network("reset".into())
    }
    fn ecancel() -> LlmError {
        LlmError::Cancelled
    }
    fn e400() -> LlmError {
        LlmError::Http {
            status: 400,
            message: "bad".into(),
            retry_after: None,
        }
    }

    fn ok_stream(text: &str) -> Step {
        Step::Items(vec![
            Ok(StreamEvent::TextDelta { text: text.into() }),
            Ok(StreamEvent::Usage(Usage {
                input_tokens: 1_000_000,
                output_tokens: 500_000,
                ..Usage::default()
            })),
            Ok(StreamEvent::Finish(FinishReason::Stop)),
        ])
    }

    fn req(model: &str) -> ChatRequest {
        ChatRequest::new(
            model,
            vec![Message {
                role: Role::User,
                content: "hello world".into(),
                reasoning: None,
                tool_calls: vec![],
                tool_call_id: None,
            }],
        )
    }

    fn fast() -> RetryPolicy {
        RetryPolicy {
            max_retries: 1,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(5),
        }
    }

    fn router(a: &Arc<Mock>, b: &Arc<Mock>) -> Router {
        Router::new()
            .provider(a.clone())
            .provider(b.clone())
            .retry(fast())
            .chain("build", &["a/m1", "b/m2"])
            .unwrap()
    }

    async fn collect(s: EventStream) -> Vec<Result<StreamEvent>> {
        s.collect().await
    }

    fn text_of(evs: &[Result<StreamEvent>]) -> String {
        evs.iter()
            .filter_map(|e| match e {
                Ok(StreamEvent::TextDelta { text }) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn router_falls_back_on_5xx_after_retry() {
        let a = Mock::new("a", vec![Step::Fail(e500), Step::Fail(e500)]);
        let b = Mock::new("b", vec![ok_stream("from-b")]);
        let r = router(&a, &b);
        let s = r
            .stream(req("build"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(text_of(&collect(s).await), "from-b");
        assert_eq!(a.calls(), 2, "1 try + 1 retry");
        assert_eq!(b.calls(), 1);
        assert_eq!(*a.models_seen.lock().unwrap(), ["m1", "m1"]);
        assert_eq!(*b.models_seen.lock().unwrap(), ["m2"]);
    }

    #[tokio::test]
    async fn router_retry_recovers_without_fallback() {
        let a = Mock::new("a", vec![Step::Fail(enet), ok_stream("from-a")]);
        let b = Mock::new("b", vec![]);
        let r = router(&a, &b);
        let s = r
            .stream(req("build"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(text_of(&collect(s).await), "from-a");
        assert_eq!(b.calls(), 0);
    }

    #[tokio::test]
    async fn router_falls_back_on_first_event_timeout() {
        let a = Mock::new("a", vec![Step::Hang, Step::Hang]);
        let b = Mock::new("b", vec![ok_stream("from-b")]);
        let r = router(&a, &b).first_event_timeout(Some(Duration::from_millis(20)));
        let s = r
            .stream(req("build"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(text_of(&collect(s).await), "from-b");
    }

    #[tokio::test]
    async fn router_falls_back_when_error_is_first_stream_item() {
        let a = Mock::new(
            "a",
            vec![Step::Items(vec![Err(e500)]), Step::Items(vec![Err(e500)])],
        );
        let b = Mock::new("b", vec![ok_stream("from-b")]);
        let r = router(&a, &b);
        let s = r
            .stream(req("build"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(text_of(&collect(s).await), "from-b");
    }

    #[tokio::test]
    async fn router_no_fallback_after_stream_started() {
        let a = Mock::new(
            "a",
            vec![Step::Items(vec![
                Ok(StreamEvent::TextDelta { text: "par".into() }),
                Err(e500),
            ])],
        );
        let b = Mock::new("b", vec![ok_stream("from-b")]);
        let r = router(&a, &b);
        let s = r
            .stream(req("build"), CancellationToken::new())
            .await
            .unwrap();
        let evs = collect(s).await;
        assert_eq!(text_of(&evs), "par");
        assert!(matches!(
            evs.last(),
            Some(Err(LlmError::Http { status: 503, .. }))
        ));
        assert_eq!((a.calls(), b.calls()), (1, 0), "no retry, no fallback");
    }

    #[tokio::test]
    async fn router_auth_not_retried_but_next_route_tried() {
        let a = Mock::new("a", vec![Step::Fail(e401)]);
        let b = Mock::new("b", vec![ok_stream("from-b")]);
        let r = router(&a, &b);
        let s = r
            .stream(req("build"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(text_of(&collect(s).await), "from-b");
        assert_eq!(a.calls(), 1, "auth never retried on same route");
    }

    #[tokio::test]
    async fn router_non_retryable_4xx_not_retried() {
        let a = Mock::new("a", vec![Step::Fail(e400)]);
        let b = Mock::new("b", vec![Step::Fail(e401)]);
        let r = router(&a, &b);
        let err = r
            .stream(req("build"), CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert!(matches!(err, LlmError::Auth(_)), "last route's error wins");
        assert_eq!((a.calls(), b.calls()), (1, 1));
    }

    #[tokio::test]
    async fn router_cancelled_stops_chain() {
        let a = Mock::new("a", vec![Step::Fail(ecancel)]);
        let b = Mock::new("b", vec![ok_stream("from-b")]);
        let r = router(&a, &b);
        let err = r
            .stream(req("build"), CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert!(matches!(err, LlmError::Cancelled));
        assert_eq!((a.calls(), b.calls()), (1, 0));
    }

    #[tokio::test]
    async fn router_cancel_interrupts_hang_and_backoff() {
        let a = Mock::new("a", vec![Step::Hang]);
        let b = Mock::new("b", vec![]);
        let r = router(&a, &b).first_event_timeout(None);
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            c2.cancel();
        });
        let err = r.stream(req("build"), cancel).await.err().unwrap();
        assert!(matches!(err, LlmError::Cancelled));
        assert_eq!(b.calls(), 0);
    }

    #[tokio::test]
    async fn router_resolves_direct_ref_default_and_unknown() {
        let a = Mock::new("a", vec![ok_stream("direct")]);
        let b = Mock::new("b", vec![ok_stream("dflt")]);
        let r = router(&a, &b).default_chain(&["b/m9"]).unwrap();
        let s = r
            .stream(req("a/org/model"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(text_of(&collect(s).await), "direct");
        assert_eq!(*a.models_seen.lock().unwrap(), ["org/model"]);
        let s = r
            .stream(req("whatever"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(text_of(&collect(s).await), "dflt");

        let none = Router::new().provider(a.clone());
        let err = none
            .stream(req("x"), CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert!(matches!(err, LlmError::InvalidRequest(_)));
    }

    #[test]
    fn router_chain_rejects_unknown_provider_and_bad_ref() {
        let a = Mock::new("a", vec![]);
        let r = Router::new().provider(a);
        assert!(r.clone().chain("k", &["zzz/m"]).is_err());
        assert!(r.clone().chain("k", &["nomodel"]).is_err());
        assert!(r.chain::<&str>("k", &[]).is_err());
    }

    fn priced_catalog() -> Arc<Catalog> {
        let mut c = Catalog::default();
        c.add_custom(&CustomEndpoint {
            id: "a".into(),
            name: None,
            base_url: "http://x".into(),
            api_key_env: None,
            models: vec![CustomModel {
                id: "m1".into(),
                name: None,
                context_window: 1000,
                max_output: None,
                capabilities: Capabilities::default(), // no tools
                cost: Some(ModelCost {
                    input: 1.0,
                    output: 2.0,
                    cache_read: None,
                    cache_write: None,
                }),
            }],
        });
        Arc::new(c)
    }

    #[tokio::test]
    async fn router_budget_skips_priced_routes() {
        let a = Mock::new("a", vec![ok_stream("from-a")]);
        let b = Mock::new("b", vec![ok_stream("from-b")]);
        let r = router(&a, &b).catalog(priced_catalog()).budget(Budget {
            limit_usd: 0.0,
            hard_stop: false,
            external_spend: None,
        });
        let s = r
            .stream(req("build"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(text_of(&collect(s).await), "from-b");
        assert_eq!(a.calls(), 0);
    }

    #[test]
    fn auto_classifies_tasks() {
        let a = AutoRoutes {
            chains: HashMap::new(),
            long_context_tokens: 5,
        };
        assert_eq!(a.classify(&req("x")), TaskKind::Cheap);
        let mut r = req("x");
        r.messages[0].content = "why does this fail? ```fn main(){}```".into();
        assert_eq!(a.classify(&r), TaskKind::LongContext);
        let a2 = AutoRoutes {
            long_context_tokens: 0,
            ..a
        };
        assert_eq!(a2.classify(&r), TaskKind::Code);
    }

    #[tokio::test]
    async fn router_meters_usage_and_cost_per_route() {
        let a = Mock::new("a", vec![ok_stream("x")]);
        let b = Mock::new("b", vec![]);
        let r = router(&a, &b).catalog(priced_catalog());
        let s = r
            .stream(req("build"), CancellationToken::new())
            .await
            .unwrap();
        drop(collect(s).await);
        let m = r.meter();
        assert_eq!(m.usage.input_tokens, 1_000_000);
        // 1M in * $1 + 0.5M out * $2 per M
        assert!((m.cost.usd - 2.0).abs() < 1e-9);
        assert_eq!(m.by_route["a/m1"].requests, 1);
        assert_eq!(m.unpriced_requests, 0);
        assert_eq!(r.with_fresh_meter().meter(), Meter::default());
    }

    #[tokio::test]
    async fn router_meters_unpriced_and_partial_streams() {
        let a = Mock::new(
            "a",
            vec![Step::Items(vec![
                Ok(StreamEvent::Usage(Usage {
                    input_tokens: 7,
                    ..Usage::default()
                })),
                Err(e500),
            ])],
        );
        let b = Mock::new("b", vec![]);
        let r = router(&a, &b); // no catalog
        let s = r
            .stream(req("build"), CancellationToken::new())
            .await
            .unwrap();
        drop(collect(s).await);
        let m = r.meter();
        assert_eq!(m.usage.input_tokens, 7);
        assert_eq!(m.unpriced_requests, 1);
        assert!(m.cost.usd.abs() < f64::EPSILON);
    }

    #[test]
    fn router_projects_cost_on_primary_route() {
        let a = Mock::new("a", vec![]);
        let b = Mock::new("b", vec![]);
        let r = router(&a, &b).catalog(priced_catalog());
        let p = r.project(&req("build"), 1_000_000).unwrap();
        assert_eq!(p.input_tokens, 3); // 11 chars / 4, rounded up
        assert!((p.cost.usd - 2.000_003).abs() < 1e-9);
        assert!(r.project(&req("b/m2"), 10).is_none(), "unpriced");
    }

    #[tokio::test]
    async fn router_skips_route_without_tool_support() {
        let a = Mock::new("a", vec![]);
        let b = Mock::new("b", vec![ok_stream("from-b")]);
        let r = router(&a, &b).catalog(priced_catalog());
        let mut rq = req("build");
        rq.tools.push(crate::ToolSpec {
            name: "t".into(),
            description: "d".into(),
            parameters: serde_json::json!({}),
        });
        let s = r.stream(rq, CancellationToken::new()).await.unwrap();
        assert_eq!(text_of(&collect(s).await), "from-b");
        assert_eq!(a.calls(), 0);
    }
}
