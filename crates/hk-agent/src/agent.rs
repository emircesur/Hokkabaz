use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use hk_core::{Effect, EventBus, JobHandle, JobId, JobKind, JobRegistry, JobSpec};
use hk_llm::{
    CancellationToken, ChatRequest, FinishReason, LlmError, Provider, StreamEvent, ToolCall,
    ToolSpec, Usage,
};
use hk_session::{
    EventKind, RequestHeader, SessionEvent, SessionWriter, TurnEndReason, derive_messages,
    new_session_id, read_log,
};
use hk_tools::{ToolCtx, ToolError, ToolOutput, ToolPipeline, ToolPreExecute};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;

use crate::approval::{Gate, GateEvent};
use crate::compaction::{self, COMPACT_PROMPT, SUMMARY_PREFIX};
use crate::config::{AgentConfig, CompactionConfig};
use crate::events::{AgentPreStep, AgentRequest, AgentStream, ApprovalRequest, ApprovalResponse};
use crate::guard::{self, RepeatAction, RepeatGuard, until};
use crate::inbox::AgentHandle;
use crate::subagent::TASK_TOOL;

const CANCELLED_RESULT: &str = "tool call cancelled before it produced a result";
const SKIPPED_RESULT: &str = "tool call skipped by the user; continue without its result";
/// `assistant/attempt` error of a generation stopped by the user (partial text is kept).
pub const STOPPED_BY_USER: &str = "stopped by user";
const INTERRUPTED_RESULT: &str = "tool call interrupted: no result was recorded";

/// Job key of a tool call, so the tool (e.g. `task`) can nest its own jobs under it.
pub(crate) fn tool_job_key(session_id: &str, call_id: &str) -> String {
    format!("tool:{session_id}:{call_id}")
}

/// Close a turn job by how the turn ended.
fn end_job(job: JobHandle, reason: TurnEndReason, error: Option<&str>) {
    match reason {
        TurnEndReason::Completed => job.finish(),
        TurnEndReason::MaxSteps => {
            job.log("step cap reached");
            job.finish();
        }
        TurnEndReason::Cancelled => job.cancelled(),
        TurnEndReason::Paused => {
            job.log("paused by user");
            job.finish();
        }
        TurnEndReason::Failed => job.fail(error.unwrap_or("turn failed")),
    }
}

/// Fatal loop failure: the session log can no longer be trusted or written.
/// Provider and tool failures are not errors here; they end the turn as `failed` or become
/// error tool results.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Session log read/write failed.
    #[error("session log: {0}")]
    Session(#[from] hk_session::SessionError),
    /// Mode policy interceptor could not be installed.
    #[error("policy: {0}")]
    Policy(#[from] hk_core::CoreError),
}

type Result<T> = std::result::Result<T, AgentError>;

/// What a finished turn looked like.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    /// Turn id as logged.
    pub turn_id: String,
    /// Why it ended.
    pub reason: TurnEndReason,
    /// Steps started.
    pub steps: u32,
    /// Usage summed over the turn's steps.
    pub usage: Usage,
    /// Failure text when `reason` is `failed`.
    pub error: Option<String>,
}

/// Live LLM telemetry for the step job: TTFT, throughput, phase, tokens, context size.
struct StreamMeter {
    started: Instant,
    first: Option<Instant>,
    /// Streamed characters; ~4 per token until the provider reports usage.
    chars: usize,
    usage: Option<Usage>,
    context_tokens: u64,
    phase: &'static str,
}

impl StreamMeter {
    fn new(context_tokens: u64) -> Self {
        Self {
            started: Instant::now(),
            first: None,
            chars: 0,
            usage: None,
            context_tokens,
            phase: "waiting",
        }
    }

    fn observe(&mut self, ev: &StreamEvent) -> Option<Value> {
        let now = Instant::now();
        let (n, phase) = match ev {
            StreamEvent::TextDelta { text } => (text.len(), "writing"),
            StreamEvent::ReasoningDelta { text } => (text.len(), "thinking"),
            StreamEvent::ToolCallDelta {
                arguments_delta, ..
            } => (arguments_delta.len(), "tool_args"),
            StreamEvent::Usage(u) => {
                self.usage = Some(*u);
                (0, self.phase)
            }
            _ => return None,
        };
        if n > 0 {
            self.first.get_or_insert(now);
            self.chars += n;
        }
        self.phase = phase;
        let out = self
            .usage
            .as_ref()
            .map_or((self.chars / 4) as u64, |u| u.output_tokens);
        let gen_s = self
            .first
            .map_or(0.0, |f| now.duration_since(f).as_secs_f64());
        Some(json!({
            "phase": self.phase,
            "ttft_ms": self.first.map(|f| f.duration_since(self.started).as_millis() as u64),
            "elapsed_ms": now.duration_since(self.started).as_millis() as u64,
            "tokens_out": out,
            "tokens_estimated": self.usage.is_none(),
            "tokens_per_s": if gen_s > 0.2 { (out as f64 / gen_s * 10.0).round() / 10.0 } else { 0.0 },
            "input_tokens": self.usage.as_ref().map(Usage::total_input),
            "reasoning_tokens": self.usage.as_ref().map(|u| u.reasoning_tokens),
            "cached_tokens": self.usage.as_ref().map(|u| u.cache_read_tokens),
            "context_tokens": self.context_tokens,
        }))
    }
}

/// Why a step stopped early.
enum Stop {
    Cancelled,
    Failed(String),
}

/// Why a model request ended without a finish.
enum Fail {
    Llm(LlmError),
    /// A guard deadline passed; the text names which.
    Timeout(&'static str),
}

/// Tool calls of these names in one reply run concurrently (the subagent limiter bounds them).
fn parallel_tool(name: &str) -> bool {
    name == TASK_TOOL
}

/// How one batch of tool calls ended.
enum BatchEnd {
    /// `None` = skipped by the user.
    Done(Vec<Option<std::result::Result<ToolOutput, ToolError>>>),
    Cancelled,
    /// Per-call timeout: the calls get error results, the turn goes on.
    ToolTimeout(&'static str),
    TurnTimeout,
    Gate(Option<GateEvent>),
}

struct StepOut {
    calls: Vec<ToolCall>,
    finish: Option<FinishReason>,
    usage: Option<Usage>,
}

#[derive(Default)]
struct PartialCall {
    id: Option<String>,
    name: Option<String>,
    args: String,
}

/// One agent bound to one session log.
pub struct Agent {
    cfg: AgentConfig,
    provider: Arc<dyn Provider>,
    tools: ToolPipeline,
    bus: EventBus,
    writer: SessionWriter,
    /// In-memory mirror of the log; `derive_messages` runs over this.
    events: Vec<SessionEvent>,
    handle: AgentHandle,
    gate: Arc<Gate>,
    gate_rx: UnboundedReceiver<GateEvent>,
    /// Approvals logged as requested but not yet as answered.
    open_approvals: Vec<String>,
    /// Identical-call streak of the running turn.
    repeat: RepeatGuard,
    /// Job tree to report into, and the parent job turns nest under.
    jobs: Option<(JobRegistry, Option<JobId>)>,
    turn_job: Option<JobId>,
    step_job: Option<JobHandle>,
    /// Keeps the mode-policy interceptor installed for the agent's lifetime.
    _policy: Effect,
}

/// Runs after every other `tools/pre-execute` interceptor so approval is for the final args.
const POLICY_PRIORITY: i32 = 1000;

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("session", &self.writer.session_id())
            .field("model", &self.cfg.model)
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// Bind to `writer`'s session, loading its existing history.
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: ToolPipeline,
        bus: EventBus,
        cfg: AgentConfig,
        writer: SessionWriter,
    ) -> Result<Self> {
        let events = read_log(writer.path())?;
        let (gate, gate_rx) = Gate::new(cfg.mode);
        let g = gate.clone();
        let policy = bus.intercept::<ToolPreExecute, _, _>(POLICY_PRIORITY, move |e| {
            let g = g.clone();
            async move { g.check(e).await }
        })?;
        Ok(Self {
            cfg,
            provider,
            tools,
            bus,
            writer,
            events,
            handle: AgentHandle::new(gate.clone()),
            gate,
            gate_rx,
            open_approvals: Vec::new(),
            repeat: RepeatGuard::default(),
            jobs: None,
            turn_job: None,
            step_job: None,
            _policy: policy,
        })
    }

    /// Control surface (submit / inject / cancel) usable from other tasks.
    pub fn handle(&self) -> AgentHandle {
        self.handle.clone()
    }

    /// Report turns, steps and tool calls as jobs, nested under `parent`. Cancelling the turn
    /// job cancels the turn exactly like [`AgentHandle::cancel`].
    pub fn set_jobs(&mut self, reg: JobRegistry, parent: Option<JobId>) {
        self.jobs = Some((reg, parent));
    }

    fn start_job(&self, spec: JobSpec, parent: Option<JobId>) -> Option<JobHandle> {
        self.jobs.as_ref().map(|(reg, _)| reg.start(spec, parent))
    }

    /// Replace settings between turns (mode or model switch).
    pub fn set_config(&mut self, cfg: AgentConfig) {
        self.gate.set_mode(cfg.mode);
        self.gate.set_policy(cfg.policy.clone());
        self.cfg = cfg;
    }

    /// Events logged so far (including history from before this agent was created).
    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }

    /// Run turns until the inbox is empty.
    pub async fn run_until_idle(&mut self) -> Result<Vec<TurnSummary>> {
        let mut out = Vec::new();
        while let Some(s) = self.run_turn().await? {
            out.push(s);
        }
        Ok(out)
    }

    /// Claim the inbox and run one turn. `None` when the inbox was empty.
    pub async fn run_turn(&mut self) -> Result<Option<TurnSummary>> {
        // Token first so a cancel() racing the claim still hits this turn.
        let mut cancel = self.handle.begin_turn();
        let msgs = self.handle.claim_all();
        if msgs.is_empty() {
            self.handle.end_turn();
            return Ok(None);
        }
        let parent = self.jobs.as_ref().and_then(|(_, p)| *p);
        let spec = JobSpec::new(JobKind::AgentTurn, "agent turn").meta(json!({
            "session_id": self.writer.session_id(),
            "model": self.cfg.model,
        }));
        let job = self.start_job(spec, parent);
        if let (Some(job), Some((reg, _))) = (&job, &self.jobs) {
            // One cancel path: the turn runs on the job's token.
            cancel = self
                .handle
                .adopt_job(job.token().clone(), reg.clone(), job.id());
        }
        self.turn_job = job.as_ref().map(JobHandle::id);
        let res = self.drive(msgs, cancel).await;
        // A step left open by a fatal error closes before its turn.
        drop(self.step_job.take());
        self.turn_job = None;
        self.handle.end_turn();
        if let Some(job) = job {
            match &res {
                Ok(s) => end_job(job, s.reason, s.error.as_deref()),
                Err(e) => job.fail(e.to_string()),
            }
        }
        res.map(Some)
    }

    fn log(&mut self, kind: EventKind) -> Result<()> {
        let ev = self.writer.append(kind)?;
        self.events.push(ev);
        Ok(())
    }

    fn log_result(
        &mut self,
        call_id: &str,
        content: impl Into<String>,
        is_error: bool,
    ) -> Result<()> {
        self.log(EventKind::ToolResult {
            call_id: call_id.to_owned(),
            content: content.into(),
            is_error,
        })
    }

    /// Heal a log left by a crash: answer calls that never got a result and close an open
    /// turn, so the next request is valid for providers that require paired tool messages.
    fn repair(&mut self) -> Result<()> {
        let mut open: Vec<String> = Vec::new();
        let mut open_turn: Option<String> = None;
        for ev in &self.events {
            match &ev.kind {
                EventKind::ToolCall { call_id, .. } => open.push(call_id.clone()),
                EventKind::ToolResult { call_id, .. } => open.retain(|c| c != call_id),
                EventKind::TurnStart { turn_id } => open_turn = Some(turn_id.clone()),
                EventKind::TurnEnd { .. } => open_turn = None,
                _ => {}
            }
        }
        for id in open {
            self.log_result(&id, INTERRUPTED_RESULT, true)?;
        }
        if let Some(turn_id) = open_turn {
            self.log(EventKind::TurnEnd {
                turn_id,
                reason: TurnEndReason::Failed,
            })?;
        }
        Ok(())
    }

    async fn drive(&mut self, msgs: Vec<String>, cancel: CancellationToken) -> Result<TurnSummary> {
        self.repair()?;
        let has_system = self
            .events
            .iter()
            .any(|e| matches!(e.kind, EventKind::SystemMessage { .. }));
        if !has_system {
            for s in self.cfg.system.clone() {
                self.log(EventKind::SystemMessage { content: s })?;
            }
        }
        let turn_id = new_session_id();
        self.log(EventKind::TurnStart {
            turn_id: turn_id.clone(),
        })?;
        for m in msgs {
            self.log(EventKind::UserMessage { content: m })?;
        }

        let mut steps = 0u32;
        let mut usage = Usage::default();
        self.repeat.reset();
        let deadline = self.cfg.guards.turn_timeout.map(|d| Instant::now() + d);
        let (reason, error) = self
            .step_loop(&turn_id, &cancel, deadline, &mut steps, &mut usage)
            .await?;
        self.log(EventKind::TurnEnd {
            turn_id: turn_id.clone(),
            reason,
        })?;
        Ok(TurnSummary {
            turn_id,
            reason,
            steps,
            usage,
            error,
        })
    }

    async fn step_loop(
        &mut self,
        turn_id: &str,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
        steps: &mut u32,
        usage: &mut Usage,
    ) -> Result<(TurnEndReason, Option<String>)> {
        loop {
            if cancel.is_cancelled() {
                return Ok((TurnEndReason::Cancelled, None));
            }
            if *steps >= self.cfg.max_steps {
                return Ok((TurnEndReason::MaxSteps, None));
            }
            let step = *steps;
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return self.guard_stop(step, "turn timeout exceeded".to_owned());
            }

            if let Some(cc) = self.cfg.compaction.clone()
                && compaction::context_tokens(&self.events) >= cc.threshold_tokens
            {
                self.compact_with(&cc, step, cancel).await?;
            }

            for m in self.handle.claim_injected() {
                self.log(EventKind::UserMessage { content: m })?;
            }
            let pre = self
                .bus
                .emit(AgentPreStep {
                    turn_id: turn_id.to_owned(),
                    step,
                    reminders: Vec::new(),
                })
                .await;
            match pre {
                Ok(p) => {
                    for r in p.reminders {
                        self.log(EventKind::UserMessage { content: r })?;
                    }
                }
                Err(r) => return Ok((TurnEndReason::Failed, Some(r.to_string()))),
            }
            match self.repeat.check(&self.cfg.guards) {
                RepeatAction::None => {}
                RepeatAction::Remind(m) => self.log(EventKind::UserMessage { content: m })?,
                RepeatAction::Abort(m) => return self.guard_stop(step, m),
            }

            self.log(EventKind::StepStart {
                turn_id: turn_id.to_owned(),
                step,
            })?;
            *steps += 1;
            let spec = JobSpec::new(JobKind::Step, format!("step {step}"))
                .meta(json!({"turn_id": turn_id, "step": step}));
            self.step_job = self.start_job(spec, self.turn_job);
            if let Some(j) = &self.step_job {
                j.phase("model");
            }

            let (out, mut stop) = self.model_phase(turn_id, step, cancel, deadline).await?;
            for c in &out.calls {
                self.repeat.observe(&c.name, &c.arguments);
            }
            if stop.is_none() && !out.calls.is_empty() {
                if let Some(j) = &self.step_job {
                    j.phase("tools");
                }
                stop = self.tool_phase(&out.calls, cancel, deadline).await?;
            }
            if let Some(u) = &out.usage {
                usage.accumulate(u);
            }
            self.log(EventKind::StepEnd {
                turn_id: turn_id.to_owned(),
                step,
                finish: out.finish,
                usage: out.usage,
            })?;
            if let Some(j) = self.step_job.take() {
                match &stop {
                    Some(Stop::Cancelled) => j.cancelled(),
                    Some(Stop::Failed(e)) => j.fail(e.as_str()),
                    None => j.finish(),
                }
            }

            match stop {
                Some(Stop::Cancelled) => return Ok((TurnEndReason::Cancelled, None)),
                Some(Stop::Failed(e)) => return Ok((TurnEndReason::Failed, Some(e))),
                None => {}
            }
            if self.handle.take_pause() {
                return Ok((TurnEndReason::Paused, None));
            }
            // A steer that arrived while the model was finishing must not be dropped.
            if out.calls.is_empty() && !self.handle.has_injected() {
                return Ok((TurnEndReason::Completed, None));
            }
            // Checked only when the turn would go on: a finished answer is never thrown away.
            let g = &self.cfg.guards;
            if let (Some(cap), Some(price)) = (g.max_cost_usd, g.price) {
                let spent = price.price(usage).usd;
                if spent > cap {
                    return self.guard_stop(
                        step,
                        format!("cost cap exceeded: ${spent:.4} spent of ${cap:.4}"),
                    );
                }
            }
        }
    }

    /// End the turn as failed for a guard, leaving the reason in the log (there is no
    /// dedicated event; `assistant/attempt` is the log's failure record).
    fn guard_stop(&mut self, step: u32, msg: String) -> Result<(TurnEndReason, Option<String>)> {
        self.log(EventKind::AssistantAttempt {
            step,
            attempt: 1,
            error: Some(msg.clone()),
        })?;
        Ok((TurnEndReason::Failed, Some(msg)))
    }

    /// Summarize old history now, regardless of the threshold. `false` when there is nothing
    /// to compact yet or the summary request failed (the log is then unchanged). Between turns.
    pub async fn compact(&mut self) -> Result<bool> {
        let cc = self
            .cfg
            .compaction
            .clone()
            .unwrap_or_else(|| CompactionConfig::new(0));
        let step = 0;
        self.compact_with(&cc, step, &CancellationToken::new())
            .await
    }

    /// One summarization round. Failures never fail the turn: the next request just carries
    /// the longer history (and may hit a context limit, which the provider reports).
    async fn compact_with(
        &mut self,
        cc: &CompactionConfig,
        step: u32,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let Some(cut) = compaction::plan_cut(&self.events, cc.keep_recent_steps) else {
            return Ok(false);
        };
        let mut msgs = derive_messages(&self.events[..cut.index]);
        if msgs.iter().all(|m| m.role == hk_llm::Role::System) {
            return Ok(false);
        }
        msgs.push(compaction::user(COMPACT_PROMPT));
        let req = ChatRequest::new(self.cfg.model.clone(), msgs);
        let timeout = self.cfg.guards.step_timeout;
        let summary = {
            let run = self.summarize(req, cancel);
            match timeout {
                Some(t) => tokio::time::timeout(t, run)
                    .await
                    .unwrap_or_else(|_| Err(LlmError::Network("compaction timed out".into()))),
                None => run.await,
            }
        };
        match summary {
            Ok(text) if !text.trim().is_empty() => {
                self.log(EventKind::CompactionSummary {
                    summary: format!("{SUMMARY_PREFIX}{}", text.trim()),
                    through_seq: cut.through_seq,
                })?;
                Ok(true)
            }
            Ok(_) => {
                tracing::warn!("compaction: model returned an empty summary; skipped");
                Ok(false)
            }
            // The loop's own cancel check ends the turn.
            Err(LlmError::Cancelled) => Ok(false),
            Err(e) => {
                tracing::warn!(error = %e, "compaction failed; continuing uncompacted");
                self.log(EventKind::AssistantAttempt {
                    step,
                    attempt: 1,
                    error: Some(format!("compaction failed: {e}")),
                })?;
                Ok(false)
            }
        }
    }

    async fn summarize(
        &self,
        req: ChatRequest,
        cancel: &CancellationToken,
    ) -> std::result::Result<String, LlmError> {
        let mut stream = self.provider.stream(req, cancel.clone()).await?;
        let mut text = String::new();
        loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(LlmError::Cancelled),
                n = stream.next() => n,
            };
            match next {
                Some(Ok(StreamEvent::TextDelta { text: t })) => text.push_str(&t),
                Some(Ok(StreamEvent::Finish(_))) => return Ok(text),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e),
                None => return Err(LlmError::Protocol("stream ended without finish".into())),
            }
        }
    }

    /// Build + send the request, stream it, log assistant text and tool calls.
    async fn model_phase(
        &mut self,
        turn_id: &str,
        step: u32,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<(StepOut, Option<Stop>)> {
        let lim = guard::limit(
            deadline,
            self.cfg.guards.step_timeout.map(|d| (d, "step timeout")),
        );
        let at = lim.map(|l| l.0);
        let mut out = StepOut {
            calls: Vec::new(),
            finish: None,
            usage: None,
        };
        let derived = derive_messages(&self.events);
        let mut request = ChatRequest::new(self.cfg.model.clone(), derived.clone());
        request.tools = self
            .tools
            .registry()
            .specs()
            .into_iter()
            .filter(|s| self.gate.mode().exposes(&s.name))
            .map(to_llm_spec)
            .collect();
        request.tool_choice = self.cfg.tool_choice.clone();
        request.temperature = self.cfg.temperature;
        request.max_output_tokens = self.cfg.max_output_tokens;
        request.reasoning_effort = self.cfg.reasoning_effort;
        request.sampling = self.cfg.sampling.clone();

        let ev = AgentRequest {
            turn_id: turn_id.to_owned(),
            step,
            request,
        };
        let mut request = match self.bus.emit(ev).await {
            Ok(e) => e.request,
            Err(r) => return Ok((out, Some(Stop::Failed(r.to_string())))),
        };
        if request.messages != derived {
            tracing::warn!(
                "agent/request interceptor changed messages; discarded (log is the source)"
            );
            request.messages = derived;
        }
        self.log(EventKind::RequestHeader(RequestHeader {
            model: request.model.clone(),
            tools: request.tools.clone(),
            tool_choice: request.tool_choice.clone(),
            temperature: request.temperature,
            max_output_tokens: request.max_output_tokens,
            reasoning_effort: request.reasoning_effort,
            message_count: request.messages.len() as u32,
        }))?;

        let started = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(Fail::Llm(LlmError::Cancelled)),
            () = until(at) => Err(Fail::Timeout(lim.map_or("timeout", |l| l.1))),
            r = self.provider.stream(request, cancel.clone()) => r.map_err(Fail::Llm),
        };
        let mut stream = match started {
            Ok(s) => s,
            Err(e) => return self.request_failed(step, out, e).await,
        };

        let mut text = String::new();
        let mut reasoning = String::new();
        let mut partial: BTreeMap<u32, PartialCall> = BTreeMap::new();
        let mut failure: Option<Fail> = None;
        let mut meter = StreamMeter::new(compaction::context_tokens(&self.events));
        loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => Some(Err(Fail::Llm(LlmError::Cancelled))),
                () = until(at) => Some(Err(Fail::Timeout(lim.map_or("timeout", |l| l.1)))),
                n = stream.next() => n.map(|r| r.map_err(Fail::Llm)),
            };
            let ev = match next {
                None => {
                    if out.finish.is_none() {
                        failure = Some(Fail::Llm(LlmError::Protocol(
                            "stream ended without finish".into(),
                        )));
                    }
                    break;
                }
                Some(Err(e)) => {
                    failure = Some(e);
                    break;
                }
                Some(Ok(ev)) => ev,
            };
            // Listener-only traffic; a reject can't mean anything for a notification.
            let _ = self
                .bus
                .emit(AgentStream {
                    turn_id: turn_id.to_owned(),
                    step,
                    event: ev.clone(),
                })
                .await;
            if let Some(j) = &self.step_job
                && let Some(m) = meter.observe(&ev)
            {
                j.metrics(m, matches!(ev, StreamEvent::Usage(_)));
            }
            match ev {
                StreamEvent::TextDelta { text: t } => text.push_str(&t),
                StreamEvent::ReasoningDelta { text: t } => reasoning.push_str(&t),
                StreamEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments_delta,
                } => {
                    let p = partial.entry(index).or_default();
                    p.id = p.id.take().or(id);
                    p.name = p.name.take().or(name);
                    p.args.push_str(&arguments_delta);
                }
                StreamEvent::Usage(u) => out.usage = Some(u),
                StreamEvent::Finish(r) => {
                    out.finish = Some(r);
                    break;
                }
            }
        }

        // Cut-off streams may carry truncated tool arguments; only complete responses
        // contribute calls, partial text is kept so the user's view and the log agree.
        if failure.is_none() {
            out.calls = partial
                .into_iter()
                .map(|(i, p)| finish_call(step, i, p))
                .collect();
        }
        if !text.is_empty() || !reasoning.is_empty() || !out.calls.is_empty() {
            self.log(EventKind::AssistantMessage {
                content: text,
                reasoning: (!reasoning.is_empty()).then_some(reasoning),
            })?;
            for c in out.calls.clone() {
                self.log(EventKind::ToolCall {
                    call_id: c.id,
                    name: c.name,
                    arguments: c.arguments,
                })?;
            }
        }
        match failure {
            None => Ok((out, None)),
            Some(e) => self.request_failed(step, out, e).await,
        }
    }

    async fn request_failed(
        &mut self,
        step: u32,
        out: StepOut,
        e: Fail,
    ) -> Result<(StepOut, Option<Stop>)> {
        let msg = match e {
            Fail::Llm(LlmError::Cancelled) => {
                self.log(EventKind::AssistantAttempt {
                    step,
                    attempt: 1,
                    error: Some(STOPPED_BY_USER.to_owned()),
                })?;
                return Ok((out, Some(Stop::Cancelled)));
            }
            Fail::Llm(e) => e.to_string(),
            Fail::Timeout(what) => format!("{what} exceeded"),
        };
        self.log(EventKind::AssistantAttempt {
            step,
            attempt: 1,
            error: Some(msg.clone()),
        })?;
        Ok((out, Some(Stop::Failed(msg))))
    }

    /// Log an approval event and announce it on the bus (listener-only).
    async fn log_gate(&mut self, ev: GateEvent) -> Result<()> {
        match ev {
            GateEvent::Request {
                approval_id,
                call_id,
                tool,
                risk,
                detail,
            } => {
                self.open_approvals.push(approval_id.clone());
                self.log(EventKind::ApprovalRequest {
                    approval_id: approval_id.clone(),
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    detail: detail.clone(),
                })?;
                let _ = self
                    .bus
                    .emit(ApprovalRequest {
                        approval_id,
                        call_id,
                        tool,
                        risk,
                        detail,
                    })
                    .await;
            }
            GateEvent::Response {
                approval_id,
                approved,
                reason,
            } => {
                self.open_approvals.retain(|a| *a != approval_id);
                self.log(EventKind::ApprovalResponse {
                    approval_id: approval_id.clone(),
                    approved,
                    reason: reason.clone(),
                })?;
                let _ = self
                    .bus
                    .emit(ApprovalResponse {
                        approval_id,
                        approved,
                        reason,
                    })
                    .await;
            }
        }
        Ok(())
    }

    /// A cancelled turn leaves parked approvals unanswered; close them as denied so the log
    /// never shows a request that is still "pending".
    async fn cancel_open_approvals(&mut self) -> Result<()> {
        for approval_id in std::mem::take(&mut self.open_approvals) {
            self.log_gate(GateEvent::Response {
                approval_id,
                approved: false,
                reason: Some("turn cancelled".to_owned()),
            })
            .await?;
        }
        Ok(())
    }

    /// Run calls through the pipeline in order; consecutive `task` calls run together as one
    /// batch. Every call gets exactly one result, also after a cancel or timeout (the
    /// remaining ones are answered, not skipped).
    async fn tool_phase(
        &mut self,
        calls: &[ToolCall],
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Result<Option<Stop>> {
        let tools = self.tools.clone();
        let mut stop = None;
        let mut i = 0;
        while i < calls.len() {
            let mut j = i + 1;
            if parallel_tool(&calls[i].name) {
                while j < calls.len() && parallel_tool(&calls[j].name) {
                    j += 1;
                }
            }
            let batch = &calls[i..j];
            i = j;
            if stop.is_some() {
                for call in batch {
                    self.log_result(&call.id, CANCELLED_RESULT, true)?;
                }
                continue;
            }

            // Subagents are bounded by their own guards; a parked approval is the user's time.
            let per_call = if parallel_tool(&batch[0].name) {
                None
            } else {
                self.cfg.guards.tool_timeout
            };
            let session_id = self.writer.session_id().to_owned();
            let cwd = self.cfg.cwd.clone();
            // Started before the calls run so a tool can find its job by key.
            let step_job = self.step_job.as_ref().map(JobHandle::id);
            let jobs: Vec<Option<JobHandle>> = batch
                .iter()
                .map(|call| {
                    let spec = JobSpec::new(JobKind::Tool, call.name.clone())
                        .key(tool_job_key(&session_id, &call.id))
                        .meta(json!({"call_id": call.id, "tool": call.name}));
                    self.start_job(spec, step_job)
                })
                .collect();
            // Cancelling one tool's job skips just that call; the turn goes on.
            let skips: Vec<Option<CancellationToken>> = jobs
                .iter()
                .map(|j| j.as_ref().map(|j| j.token().clone()))
                .collect();
            let run = futures::future::join_all(batch.iter().zip(skips).map(|(call, skip)| {
                let ctx = ToolCtx {
                    call_id: call.id.clone(),
                    session_id: Some(session_id.clone()),
                    cwd: cwd.clone(),
                };
                let fut = tools.run(ctx, &call.name, call.arguments.clone());
                async move {
                    match skip {
                        Some(t) => tokio::select! {
                            biased;
                            () = t.cancelled() => None,
                            r = fut => Some(r),
                        },
                        None => Some(fut.await),
                    }
                }
            }));
            tokio::pin!(run);
            let rearm = |d: Option<std::time::Duration>| d.map(|d| Instant::now() + d);
            let mut call_at = rearm(per_call);
            let end = loop {
                // Approval traffic is logged here, while the calls are parked.
                let armed = if self.open_approvals.is_empty() {
                    call_at
                } else {
                    None
                };
                let ev = tokio::select! {
                    biased;
                    () = cancel.cancelled() => BatchEnd::Cancelled,
                    () = until(deadline) => BatchEnd::TurnTimeout,
                    () = until(armed) => BatchEnd::ToolTimeout("tool call"),
                    ev = self.gate_rx.recv() => BatchEnd::Gate(ev),
                    r = &mut run => BatchEnd::Done(r),
                };
                match ev {
                    BatchEnd::Gate(ev) => {
                        if let Some(ev) = ev {
                            self.log_gate(ev).await?;
                        }
                        // The clock restarts once the human has answered.
                        call_at = rearm(per_call);
                    }
                    other => break other,
                }
            };
            // Responses queued just before the calls woke must precede their results.
            while let Ok(ev) = self.gate_rx.try_recv() {
                self.log_gate(ev).await?;
            }
            if !matches!(end, BatchEnd::Done(_)) {
                self.cancel_open_approvals().await?;
            }
            match end {
                BatchEnd::Done(rs) => {
                    for ((call, r), job) in batch.iter().zip(rs).zip(jobs) {
                        let failed = match r {
                            None => {
                                self.log_result(&call.id, SKIPPED_RESULT, true)?;
                                if let Some(j) = job {
                                    j.cancelled();
                                }
                                continue;
                            }
                            Some(Ok(o)) => {
                                self.log_result(&call.id, o.content, o.is_error)?;
                                o.is_error.then(|| "tool returned an error".to_owned())
                            }
                            Some(Err(e)) => {
                                self.log_result(&call.id, e.to_output().content, true)?;
                                Some(e.to_string())
                            }
                        };
                        match (job, failed) {
                            (Some(j), Some(e)) => j.fail(e),
                            (Some(j), None) => j.finish(),
                            (None, _) => {}
                        }
                    }
                }
                BatchEnd::Cancelled => {
                    stop = Some(Stop::Cancelled);
                    for (call, job) in batch.iter().zip(jobs) {
                        self.log_result(&call.id, CANCELLED_RESULT, true)?;
                        if let Some(j) = job {
                            j.cancelled();
                        }
                    }
                }
                BatchEnd::TurnTimeout => {
                    stop = Some(Stop::Failed("turn timeout exceeded".to_owned()));
                    for (call, job) in batch.iter().zip(jobs) {
                        self.log_result(&call.id, "tool call aborted: turn timeout", true)?;
                        if let Some(j) = job {
                            j.fail("turn timeout");
                        }
                    }
                }
                BatchEnd::ToolTimeout(what) => {
                    let secs = per_call.map_or(0, |d| d.as_secs_f64() as u64);
                    for (call, job) in batch.iter().zip(jobs) {
                        let msg = format!(
                            "{what} `{}` timed out after {secs}s and was aborted",
                            call.name
                        );
                        if let Some(j) = job {
                            j.fail(msg.as_str());
                        }
                        self.log_result(&call.id, msg, true)?;
                    }
                }
                BatchEnd::Gate(_) => unreachable!("gate events are handled in the loop"),
            }
        }
        Ok(stop)
    }
}

/// hk-tools keeps a mirror `ToolSpec` so it needn't depend on the provider layer.
fn to_llm_spec(s: hk_tools::ToolSpec) -> ToolSpec {
    ToolSpec {
        name: s.name,
        description: s.description,
        parameters: s.parameters,
    }
}

fn finish_call(step: u32, index: u32, p: PartialCall) -> ToolCall {
    let arguments = if p.args.trim().is_empty() {
        json!({})
    } else {
        // Unparseable args are kept verbatim as a string: schema validation then fails the
        // call with a readable error the model can correct.
        serde_json::from_str(&p.args).unwrap_or(Value::String(p.args))
    };
    ToolCall {
        id: p
            .id
            .filter(|i| !i.is_empty())
            .unwrap_or_else(|| format!("call_{step}_{index}")),
        name: p.name.unwrap_or_default(),
        arguments,
    }
}
