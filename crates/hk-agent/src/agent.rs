use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use hk_core::{Effect, EventBus};
use hk_llm::{
    CancellationToken, ChatRequest, FinishReason, LlmError, Provider, StreamEvent, ToolCall,
    ToolSpec, Usage,
};
use hk_session::{
    EventKind, RequestHeader, SessionEvent, SessionWriter, TurnEndReason, derive_messages,
    new_session_id, read_log,
};
use hk_tools::{ToolCtx, ToolPipeline, ToolPreExecute};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::approval::{Gate, GateEvent};
use crate::config::AgentConfig;
use crate::events::{AgentPreStep, AgentRequest, AgentStream, ApprovalRequest, ApprovalResponse};
use crate::inbox::AgentHandle;

const CANCELLED_RESULT: &str = "tool call cancelled before it produced a result";
const INTERRUPTED_RESULT: &str = "tool call interrupted: no result was recorded";

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

/// Why a step stopped early.
enum Stop {
    Cancelled,
    Failed(String),
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
            _policy: policy,
        })
    }

    /// Control surface (submit / inject / cancel) usable from other tasks.
    pub fn handle(&self) -> AgentHandle {
        self.handle.clone()
    }

    /// Replace settings between turns (mode or model switch).
    pub fn set_config(&mut self, cfg: AgentConfig) {
        self.gate.set_mode(cfg.mode);
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
        let cancel = self.handle.begin_turn();
        let msgs = self.handle.claim_all();
        if msgs.is_empty() {
            self.handle.end_turn();
            return Ok(None);
        }
        let res = self.drive(msgs, cancel).await;
        self.handle.end_turn();
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
        let (reason, error) = self
            .step_loop(&turn_id, &cancel, &mut steps, &mut usage)
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

            self.log(EventKind::StepStart {
                turn_id: turn_id.to_owned(),
                step,
            })?;
            *steps += 1;

            let (out, mut stop) = self.model_phase(turn_id, step, cancel).await?;
            if stop.is_none() && !out.calls.is_empty() {
                stop = self.tool_phase(&out.calls, cancel).await?;
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

            match stop {
                Some(Stop::Cancelled) => return Ok((TurnEndReason::Cancelled, None)),
                Some(Stop::Failed(e)) => return Ok((TurnEndReason::Failed, Some(e))),
                None => {}
            }
            // A steer that arrived while the model was finishing must not be dropped.
            if out.calls.is_empty() && !self.handle.has_injected() {
                return Ok((TurnEndReason::Completed, None));
            }
        }
    }

    /// Build + send the request, stream it, log assistant text and tool calls.
    async fn model_phase(
        &mut self,
        turn_id: &str,
        step: u32,
        cancel: &CancellationToken,
    ) -> Result<(StepOut, Option<Stop>)> {
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
            () = cancel.cancelled() => Err(LlmError::Cancelled),
            r = self.provider.stream(request, cancel.clone()) => r,
        };
        let mut stream = match started {
            Ok(s) => s,
            Err(e) => return self.request_failed(step, out, e).await,
        };

        let mut text = String::new();
        let mut reasoning = String::new();
        let mut partial: BTreeMap<u32, PartialCall> = BTreeMap::new();
        let mut failure: Option<LlmError> = None;
        loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => Some(Err(LlmError::Cancelled)),
                n = stream.next() => n,
            };
            let ev = match next {
                None => {
                    if out.finish.is_none() {
                        failure = Some(LlmError::Protocol("stream ended without finish".into()));
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
        e: LlmError,
    ) -> Result<(StepOut, Option<Stop>)> {
        if matches!(e, LlmError::Cancelled) {
            return Ok((out, Some(Stop::Cancelled)));
        }
        let msg = e.to_string();
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

    /// Run calls in order through the pipeline. Every call gets exactly one result, also
    /// after a cancel (the remaining ones are answered, not skipped).
    async fn tool_phase(
        &mut self,
        calls: &[ToolCall],
        cancel: &CancellationToken,
    ) -> Result<Option<Stop>> {
        let tools = self.tools.clone();
        let mut stop = None;
        for call in calls {
            if stop.is_some() {
                self.log_result(&call.id, CANCELLED_RESULT, true)?;
                continue;
            }
            let ctx = ToolCtx {
                call_id: call.id.clone(),
                session_id: Some(self.writer.session_id().to_owned()),
                cwd: self.cfg.cwd.clone(),
            };
            let res = {
                let run = tools.run(ctx, &call.name, call.arguments.clone());
                tokio::pin!(run);
                loop {
                    // Approval traffic is logged here, while the call is parked.
                    let step = tokio::select! {
                        biased;
                        () = cancel.cancelled() => None,
                        ev = self.gate_rx.recv() => Some(Err(ev)),
                        r = &mut run => Some(Ok(r)),
                    };
                    match step {
                        None => break None,
                        Some(Err(ev)) => {
                            if let Some(ev) = ev {
                                self.log_gate(ev).await?;
                            }
                        }
                        Some(Ok(r)) => break Some(r),
                    }
                }
            };
            // Responses queued just before the call woke must precede its result.
            while let Ok(ev) = self.gate_rx.try_recv() {
                self.log_gate(ev).await?;
            }
            if res.is_none() {
                self.cancel_open_approvals().await?;
            }
            match res {
                Some(Ok(o)) => self.log_result(&call.id, o.content, o.is_error)?,
                Some(Err(e)) => self.log_result(&call.id, e.to_output().content, true)?,
                None => {
                    stop = Some(Stop::Cancelled);
                    self.log_result(&call.id, CANCELLED_RESULT, true)?;
                }
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
