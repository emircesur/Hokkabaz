//! Subagents: the `task` tool runs a child agent in its own session and returns its answer as
//! the tool result.
//!
//! The child session's `session/start` carries a `parent` ref (parent session id + the seq of
//! the `tool/call` that spawned it), so UIs can link the two logs; the parent log only holds the
//! call and the child's final text. Children get their own bus, so the parent's approval gate
//! never sees child calls, and no `task` tool, so fan-out is one level deep.
// port: dsh packages/subagent (child session per task, result as tool result, fan-out limit)

use std::sync::Arc;

use async_trait::async_trait;
use hk_core::{Effect, EventBus};
use hk_llm::Provider;
use hk_session::{EventKind, NewSession, ParentRef, SessionStore, TurnEndReason};
use hk_tools::{
    OutputLimits, Tool, ToolCtx, ToolError, ToolOutput, ToolPipeline, ToolRegistry, ToolSpec,
};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::agent::Agent;
use crate::config::AgentConfig;
use crate::events::ApprovalRequest;
use crate::mode::Mode;

/// Tool name; also what the parent loop uses to decide a call may run concurrently.
pub(crate) const TASK_TOOL: &str = "task";

const DEFAULT_PARALLEL: usize = 4;
const NO_OUTPUT: &str = "(subagent finished without a text answer)";

/// What happens when a child's tool call needs approval. Nobody is attached to a child's
/// session, so it cannot be left parked; forwarding to the parent's UI is not done yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChildApprovals {
    /// Refuse; the model sees "denied" and can adapt. Default.
    #[default]
    Deny,
    /// Grant. Only for hosts that already trust the child's mode and sandbox.
    Allow,
}

/// Shared spawner behind the `task` tool: provider, session store, child tool set, fan-out cap.
pub struct SubagentRunner {
    provider: Arc<dyn Provider>,
    store: SessionStore,
    registry: ToolRegistry,
    limits: OutputLimits,
    base: AgentConfig,
    permits: Arc<Semaphore>,
    approvals: ChildApprovals,
}

impl std::fmt::Debug for SubagentRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentRunner")
            .field("model", &self.base.model)
            .field("free_slots", &self.permits.available_permits())
            .finish_non_exhaustive()
    }
}

impl SubagentRunner {
    /// `registry` is the tool set children draw from (a `task` entry in it is skipped);
    /// `base` is their config (model, system prompt, guards, compaction), `mode` aside.
    pub fn new(
        provider: Arc<dyn Provider>,
        store: SessionStore,
        registry: ToolRegistry,
        limits: OutputLimits,
        base: AgentConfig,
    ) -> Self {
        Self {
            provider,
            store,
            registry,
            limits,
            base,
            permits: Arc::new(Semaphore::new(DEFAULT_PARALLEL)),
            approvals: ChildApprovals::default(),
        }
    }

    /// Children running at once; extra `task` calls wait for a slot. Min 1.
    #[must_use]
    pub fn max_parallel(mut self, n: usize) -> Self {
        self.permits = Arc::new(Semaphore::new(n.max(1)));
        self
    }

    /// How children's approval requests are answered.
    #[must_use]
    pub fn approvals(mut self, a: ChildApprovals) -> Self {
        self.approvals = a;
        self
    }

    /// Children get the parent's tools minus `task`, on a registry of their own.
    fn child_pipeline(&self, bus: EventBus) -> Result<(ToolPipeline, Vec<Effect>), ToolError> {
        let reg = ToolRegistry::new();
        let mut effects = Vec::new();
        for name in self.registry.names() {
            if name == TASK_TOOL {
                continue;
            }
            if let Some(tool) = self.registry.get(&name) {
                effects.push(reg.register(tool)?);
            }
        }
        Ok((ToolPipeline::new(reg, bus, self.limits.clone()), effects))
    }

    /// Seq of the parent's `tool/call` for `call_id`, else its last seq.
    fn parent_seq(&self, parent: &str, call_id: &str) -> u64 {
        let Ok(log) = self.store.read(parent) else {
            return 0;
        };
        log.iter()
            .rev()
            .find(|e| matches!(&e.kind, EventKind::ToolCall { call_id: c, .. } if c == call_id))
            .or(log.last())
            .map_or(0, |e| e.seq)
    }

    async fn run(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        let prompt = args["prompt"].as_str().unwrap_or_default().to_owned();
        let title = args["description"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let mode = match args["mode"].as_str() {
            Some("plan") => Mode::Plan,
            _ => Mode::Build,
        };
        let parent = ctx
            .session_id
            .clone()
            .ok_or_else(|| fail("no parent session in tool context"))?;

        // Queued tasks hold no session until a slot frees up.
        let _slot = self
            .permits
            .acquire()
            .await
            .map_err(|_| fail("subagent limiter closed"))?;

        let mut cfg = self.base.clone();
        cfg.mode = mode;
        if ctx.cwd.is_some() {
            cfg.cwd = ctx.cwd.clone();
        }
        let writer = self
            .store
            .create_child(
                NewSession {
                    id: None,
                    cwd: cfg.cwd.as_ref().map(|p| p.display().to_string()),
                    model: Some(cfg.model.clone()),
                    title,
                },
                ParentRef {
                    seq: self.parent_seq(&parent, &ctx.call_id),
                    session_id: parent,
                },
            )
            .map_err(fail)?;
        let child_id = writer.session_id().to_owned();

        let bus = EventBus::new();
        let (pipeline, _tools) = self.child_pipeline(bus.clone())?;
        let mut agent =
            Agent::new(self.provider.clone(), pipeline, bus.clone(), cfg, writer).map_err(fail)?;
        let handle = agent.handle();
        let allow = self.approvals == ChildApprovals::Allow;
        // Emitted after the request is logged, so answering here is safe.
        let _answer = bus.on::<ApprovalRequest, _, _>(move |e| {
            let h = handle.clone();
            async move {
                let why = (!allow).then(|| "subagents cannot ask for approval".to_owned());
                h.respond(&e.approval_id, allow, why);
            }
        });

        agent.handle().submit(prompt);
        let turns = agent.run_until_idle().await.map_err(fail)?;

        let answer = agent.events().iter().rev().find_map(|e| match &e.kind {
            EventKind::AssistantMessage { content, .. } if !content.trim().is_empty() => {
                Some(content.trim().to_owned())
            }
            _ => None,
        });
        let last = turns.last();
        Ok(match last.map(|t| (t.reason, t.error.clone())) {
            Some((TurnEndReason::Completed, _)) => ToolOutput::ok(format!(
                "{}\n\n[subagent session {child_id}]",
                answer.as_deref().unwrap_or(NO_OUTPUT)
            )),
            Some((reason, err)) => ToolOutput::error(format!(
                "subagent ended early ({reason:?}{}); partial answer: {}\n\n[subagent session {child_id}]",
                err.map(|e| format!(": {e}")).unwrap_or_default(),
                answer.as_deref().unwrap_or("(none)")
            )),
            None => ToolOutput::error(format!(
                "subagent ran no turn\n\n[subagent session {child_id}]"
            )),
        })
    }
}

fn fail(e: impl std::fmt::Display) -> ToolError {
    ToolError::Failed {
        tool: TASK_TOOL.to_owned(),
        reason: e.to_string(),
    }
}

/// The `task` tool: hand a self-contained job to a fresh child agent.
#[derive(Debug, Clone)]
pub struct TaskTool(Arc<SubagentRunner>);

impl TaskTool {
    /// Tool backed by `runner`.
    pub fn new(runner: Arc<SubagentRunner>) -> Self {
        Self(runner)
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: TASK_TOOL.to_owned(),
            description: "Run a subagent in its own session and return its final answer. Use for \
                self-contained jobs (search, research, a bounded change); it cannot see this \
                conversation, so put everything it needs in `prompt`. Several `task` calls in one \
                reply run in parallel."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "description": {"type": "string", "description": "Short title (3-8 words)."},
                    "prompt": {"type": "string", "description": "Complete instructions for the subagent."},
                    "mode": {"type": "string", "enum": ["build", "plan"],
                        "description": "plan = read-only filesystem. Default build."}
                },
                "required": ["prompt"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> Result<ToolOutput, ToolError> {
        self.0.run(ctx, args).await
    }
}
