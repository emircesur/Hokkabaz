//! State-machine tests against a scripted fake provider (no network).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use hk_core::{Effect, EventBus, Verdict};
use hk_llm::{
    CancellationToken, ChatRequest, EventStream, FinishReason, LlmError, ModelInfo, Provider, Role,
    StreamEvent, Usage,
};
use hk_session::{EventKind, NewSession, SessionStore, TurnEndReason, derive_messages};
use hk_tools::{
    OutputLimits, Risk, Tool, ToolCtx, ToolError, ToolOutput, ToolPipeline, ToolRegistry, ToolSpec,
};
use serde_json::{Value, json};

use crate::{Agent, AgentConfig, AgentHandle, AgentPreStep, AgentRequest, ApprovalRequest, Mode};

type Script = Vec<Result<StreamEvent, LlmError>>;
type Hook = Box<dyn Fn(usize) + Send + Sync>;

/// Replays one script per request; `hang` scripts never finish (cancel must end them).
struct Fake {
    scripts: Mutex<Vec<(Script, bool)>>,
    requests: Mutex<Vec<ChatRequest>>,
    on_request: Mutex<Option<Hook>>,
}

impl Fake {
    fn new(scripts: Vec<Script>) -> Arc<Self> {
        Self::with_hang(scripts.into_iter().map(|s| (s, false)).collect())
    }
    fn with_hang(scripts: Vec<(Script, bool)>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into_iter().rev().collect()),
            requests: Mutex::default(),
            on_request: Mutex::default(),
        })
    }
    fn hook(&self, f: impl Fn(usize) + Send + Sync + 'static) {
        *self.on_request.lock().unwrap() = Some(Box::new(f));
    }
    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for Fake {
    fn id(&self) -> &str {
        "fake"
    }
    async fn models(&self) -> hk_llm::Result<Vec<ModelInfo>> {
        Ok(Vec::new())
    }
    async fn stream(
        &self,
        req: ChatRequest,
        _cancel: CancellationToken,
    ) -> hk_llm::Result<EventStream> {
        let n = {
            let mut r = self.requests.lock().unwrap();
            r.push(req);
            r.len() - 1
        };
        if let Some(h) = self.on_request.lock().unwrap().as_ref() {
            h(n);
        }
        let (script, hang) = self
            .scripts
            .lock()
            .unwrap()
            .pop()
            .expect("script exhausted");
        let s = stream::iter(script);
        Ok(if hang {
            Box::pin(futures::StreamExt::chain(s, stream::pending()))
        } else {
            Box::pin(s)
        })
    }
}

fn text(t: &str) -> Script {
    vec![
        Ok(StreamEvent::TextDelta { text: t.into() }),
        Ok(StreamEvent::Finish(FinishReason::Stop)),
    ]
}

fn call(id: &str, name: &str, args: &str) -> Script {
    vec![
        Ok(StreamEvent::ToolCallDelta {
            index: 0,
            id: Some(id.into()),
            name: Some(name.into()),
            arguments_delta: args.into(),
        }),
        Ok(StreamEvent::Finish(FinishReason::ToolCalls)),
    ]
}

struct Echo;
#[async_trait]
impl Tool for Echo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "echo".into(),
            description: "echo".into(),
            parameters: json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
        }
    }
    async fn execute(&self, _: ToolCtx, a: Value) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::ok(format!(
            "echo:{}",
            a["text"].as_str().unwrap_or("")
        )))
    }
}

struct Slow;
#[async_trait]
impl Tool for Slow {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "slow".into(),
            description: "never returns in time".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn execute(&self, _: ToolCtx, _: Value) -> Result<ToolOutput, ToolError> {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(ToolOutput::ok("late"))
    }
}

/// Stand-in for write/edit/bash: `Risk::Ask`, records that its body ran.
struct Gated {
    name: &'static str,
    ran: Arc<Mutex<Vec<String>>>,
}
#[async_trait]
impl Tool for Gated {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: "gated".into(),
            parameters: json!({"type":"object"}),
        }
    }
    fn risk(&self, _: &Value) -> Risk {
        Risk::Ask
    }
    async fn execute(&self, _: ToolCtx, _: Value) -> Result<ToolOutput, ToolError> {
        self.ran.lock().unwrap().push(self.name.to_owned());
        Ok(ToolOutput::ok(format!("{} ran", self.name)))
    }
}

struct Env {
    ran: Arc<Mutex<Vec<String>>>,
    agent: Agent,
    fake: Arc<Fake>,
    bus: EventBus,
    store: SessionStore,
    id: String,
    _dir: tempfile::TempDir,
}

fn env_with(fake: Arc<Fake>, cfg: impl FnOnce(&mut AgentConfig)) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path().join("s")).unwrap();
    let writer = store.create(NewSession::default()).unwrap();
    let id = writer.session_id().to_owned();
    let reg = ToolRegistry::new();
    std::mem::forget(reg.register(Arc::new(Echo)).unwrap());
    std::mem::forget(reg.register(Arc::new(Slow)).unwrap());
    let ran = Arc::new(Mutex::new(Vec::new()));
    for name in ["write", "edit", "bash"] {
        std::mem::forget(
            reg.register(Arc::new(Gated {
                name,
                ran: ran.clone(),
            }))
            .unwrap(),
        );
    }
    let bus = EventBus::new();
    let pipe = ToolPipeline::new(
        reg,
        bus.clone(),
        OutputLimits::new(dir.path().join("spill")),
    );
    let mut c = AgentConfig::new("m");
    cfg(&mut c);
    let agent = Agent::new(fake.clone(), pipe, bus.clone(), c, writer).unwrap();
    Env {
        ran,
        agent,
        fake,
        bus,
        store,
        id,
        _dir: dir,
    }
}

fn env(scripts: Vec<Script>) -> Env {
    env_with(Fake::new(scripts), |_| {})
}

fn names(e: &Env) -> Vec<String> {
    e.store
        .read(&e.id)
        .unwrap()
        .iter()
        .map(|ev| match &ev.kind {
            EventKind::TurnEnd { reason, .. } => format!("turn/end:{reason:?}"),
            k => serde_json::to_value(k).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned(),
        })
        .collect()
}

fn results(e: &Env) -> Vec<(String, String, bool)> {
    e.store
        .read(&e.id)
        .unwrap()
        .into_iter()
        .filter_map(|ev| match ev.kind {
            EventKind::ToolResult {
                call_id,
                content,
                is_error,
            } => Some((call_id, content, is_error)),
            _ => None,
        })
        .collect()
}

/// Every tool call in the log has exactly one result (providers reject anything else).
fn assert_paired(e: &Env) {
    let log = e.store.read(&e.id).unwrap();
    let calls: Vec<_> = log
        .iter()
        .filter_map(|ev| match &ev.kind {
            EventKind::ToolCall { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    for c in calls {
        let n = log
            .iter()
            .filter(|ev| matches!(&ev.kind, EventKind::ToolResult { call_id, .. } if *call_id == c))
            .count();
        assert_eq!(n, 1, "call {c} has {n} results");
    }
}

#[tokio::test]
async fn text_turn_logs_expected_sequence() {
    let mut e = env_with(Fake::new(vec![text("hello")]), |c| {
        c.system = vec!["be brief".into()]
    });
    e.agent.handle().submit("hi");
    let s = e.agent.run_until_idle().await.unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].reason, TurnEndReason::Completed);
    assert_eq!(s[0].steps, 1);
    assert_eq!(
        names(&e),
        [
            "session/start",
            "system/message",
            "turn/start",
            "user/message",
            "step/start",
            "request/header",
            "assistant/message",
            "step/end",
            "turn/end:Completed"
        ]
    );
    // empty inbox: nothing runs
    assert!(e.agent.run_turn().await.unwrap().is_none());
}

#[tokio::test]
async fn tool_loop_requests_replay_from_log() {
    let mut e = env(vec![call("c1", "echo", r#"{"text":"a"}"#), text("done")]);
    e.agent.handle().submit("go");
    let s = e.agent.run_turn().await.unwrap().unwrap();
    assert_eq!((s.reason, s.steps), (TurnEndReason::Completed, 2));
    assert_eq!(results(&e), [("c1".into(), "echo:a".into(), false)]);

    // "Model-visible means logged": request N == derive over the log prefix before it.
    let reqs = e.fake.requests();
    assert_eq!(reqs.len(), 2);
    let log = e.store.read(&e.id).unwrap();
    let second_header = log
        .iter()
        .enumerate()
        .filter(|(_, ev)| matches!(ev.kind, EventKind::RequestHeader(_)))
        .nth(1)
        .unwrap()
        .0;
    assert_eq!(reqs[1].messages, derive_messages(&log[..second_header]));
    let last = reqs[1].messages.last().unwrap();
    assert_eq!(last.role, Role::Tool);
    assert_eq!(last.tool_call_id.as_deref(), Some("c1"));
    assert!(reqs[1].tools.iter().any(|t| t.name == "echo"));
}

#[tokio::test]
async fn tool_args_assemble_across_deltas_and_usage_sums() {
    let u = |n| Usage {
        input_tokens: n,
        output_tokens: 1,
        ..Default::default()
    };
    let first = vec![
        Ok(StreamEvent::ToolCallDelta {
            index: 0,
            id: Some("c1".into()),
            name: Some("echo".into()),
            arguments_delta: r#"{"te"#.into(),
        }),
        Ok(StreamEvent::ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments_delta: r#"xt":"x"}"#.into(),
        }),
        Ok(StreamEvent::Usage(u(10))),
        Ok(StreamEvent::Finish(FinishReason::ToolCalls)),
    ];
    let second = vec![
        Ok(StreamEvent::Usage(u(5))),
        Ok(StreamEvent::Finish(FinishReason::Stop)),
    ];
    let mut e = env(vec![first, second]);
    e.agent.handle().submit("go");
    let s = e.agent.run_turn().await.unwrap().unwrap();
    assert_eq!(results(&e)[0].1, "echo:x");
    assert_eq!(s.usage.input_tokens, 15);
}

#[tokio::test]
async fn bad_args_and_unknown_tool_become_error_results() {
    let mut e = env(vec![
        call("c1", "echo", "{not json"),
        call("c2", "nope", "{}"),
        text("ok"),
    ]);
    e.agent.handle().submit("go");
    let s = e.agent.run_turn().await.unwrap().unwrap();
    assert_eq!(s.reason, TurnEndReason::Completed);
    let r = results(&e);
    assert!(r[0].2 && r[1].2 && r[1].1.contains("unknown tool"));
    assert_paired(&e);
}

#[tokio::test]
async fn max_steps_stops_turn_with_results_in_place() {
    let mut e = env_with(
        Fake::new(vec![
            call("a", "echo", r#"{"text":"1"}"#),
            call("b", "echo", r#"{"text":"2"}"#),
            text("never"),
        ]),
        |c| c.max_steps = 2,
    );
    e.agent.handle().submit("loop");
    let s = e.agent.run_turn().await.unwrap().unwrap();
    assert_eq!((s.reason, s.steps), (TurnEndReason::MaxSteps, 2));
    assert_eq!(e.fake.requests().len(), 2);
    assert_paired(&e);
    assert_eq!(names(&e).last().unwrap(), "turn/end:MaxSteps");
}

#[tokio::test]
async fn inject_lands_before_next_step_and_extends_a_finishing_turn() {
    let fake = Fake::new(vec![text("first"), text("second")]);
    let mut e = env_with(fake, |_| {});
    let h = e.agent.handle();
    h.submit("go");
    let h2 = h.clone();
    // Steer arrives while request 0 is in flight; the model then finishes without tools.
    e.fake.hook(move |n| {
        if n == 0 {
            h2.inject("actually do X");
        }
    });
    let s = e.agent.run_turn().await.unwrap().unwrap();
    assert_eq!((s.reason, s.steps), (TurnEndReason::Completed, 2));
    let second = &e.fake.requests()[1];
    assert_eq!(second.messages.last().unwrap().content, "actually do X");
}

#[tokio::test]
async fn submit_during_turn_queues_next_turn() {
    let fake = Fake::new(vec![text("a"), text("b")]);
    let mut e = env_with(fake, |_| {});
    let h = e.agent.handle();
    h.submit("one");
    let h2 = h.clone();
    e.fake.hook(move |n| {
        if n == 0 {
            h2.submit("two");
        }
    });
    let s = e.agent.run_until_idle().await.unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(
        e.fake.requests()[1]
            .messages
            .iter()
            .filter(|m| m.role == Role::User)
            .count(),
        2
    );
}

fn cancel_soon(h: AgentHandle) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        h.cancel();
    });
}

#[tokio::test]
async fn cancel_mid_stream_keeps_partial_text() {
    let partial = vec![Ok(StreamEvent::TextDelta {
        text: "part".into(),
    })];
    let mut e = env_with(Fake::with_hang(vec![(partial, true)]), |_| {});
    let h = e.agent.handle();
    h.submit("go");
    cancel_soon(h);
    let s = e.agent.run_turn().await.unwrap().unwrap();
    assert_eq!(s.reason, TurnEndReason::Cancelled);
    let log = e.store.read(&e.id).unwrap();
    assert!(log.iter().any(
        |ev| matches!(&ev.kind, EventKind::AssistantMessage { content, .. } if content == "part")
    ));
    assert_eq!(names(&e).last().unwrap(), "turn/end:Cancelled");
    assert!(!e.agent.handle().is_running());
}

#[tokio::test]
async fn cancel_during_tools_answers_every_call() {
    let two = vec![
        Ok(StreamEvent::ToolCallDelta {
            index: 0,
            id: Some("s1".into()),
            name: Some("slow".into()),
            arguments_delta: "{}".into(),
        }),
        Ok(StreamEvent::ToolCallDelta {
            index: 1,
            id: Some("e1".into()),
            name: Some("echo".into()),
            arguments_delta: r#"{"text":"x"}"#.into(),
        }),
        Ok(StreamEvent::Finish(FinishReason::ToolCalls)),
    ];
    let mut e = env(vec![two]);
    let h = e.agent.handle();
    h.submit("go");
    cancel_soon(h);
    let s = e.agent.run_turn().await.unwrap().unwrap();
    assert_eq!(s.reason, TurnEndReason::Cancelled);
    assert_paired(&e);
    assert!(results(&e).iter().all(|r| r.2));
    // Session stays usable: next turn's request has paired tool messages.
    e.fake.scripts.lock().unwrap().push((text("back"), false));
    e.agent.handle().submit("again");
    assert_eq!(
        e.agent.run_turn().await.unwrap().unwrap().reason,
        TurnEndReason::Completed
    );
}

#[tokio::test]
async fn stream_error_fails_turn_and_drops_truncated_calls() {
    let broken = vec![
        Ok(StreamEvent::TextDelta {
            text: "thinking".into(),
        }),
        Ok(StreamEvent::ToolCallDelta {
            index: 0,
            id: Some("c1".into()),
            name: Some("echo".into()),
            arguments_delta: r#"{"te"#.into(),
        }),
        Err(LlmError::Network("reset".into())),
    ];
    let mut e = env(vec![broken]);
    e.agent.handle().submit("go");
    let s = e.agent.run_turn().await.unwrap().unwrap();
    assert_eq!(s.reason, TurnEndReason::Failed);
    assert!(s.error.unwrap().contains("reset"));
    let n = names(&e);
    assert!(!n.contains(&"tool/call".to_string()));
    assert!(n.contains(&"assistant/attempt".to_string()));
    assert_eq!(n.last().unwrap(), "turn/end:Failed");
}

#[tokio::test]
async fn stream_without_finish_is_a_failure() {
    let mut e = env(vec![vec![Ok(StreamEvent::TextDelta { text: "x".into() })]]);
    e.agent.handle().submit("go");
    assert_eq!(
        e.agent.run_turn().await.unwrap().unwrap().reason,
        TurnEndReason::Failed
    );
}

#[tokio::test]
async fn crash_leftovers_are_repaired_on_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    let mut w = store.create(NewSession::default()).unwrap();
    let id = w.session_id().to_owned();
    for k in [
        EventKind::TurnStart {
            turn_id: "t0".into(),
        },
        EventKind::UserMessage { images: Vec::new(),
            content: "x".into(),
        },
        EventKind::AssistantMessage {
            content: String::new(),
            reasoning: None,
        },
        EventKind::ToolCall {
            call_id: "dangling".into(),
            name: "echo".into(),
            arguments: json!({}),
        },
    ] {
        w.append(k).unwrap();
    }
    drop(w);

    let fake = Fake::new(vec![text("recovered")]);
    let reg = ToolRegistry::new();
    let bus = EventBus::new();
    let pipe = ToolPipeline::new(reg, bus.clone(), OutputLimits::new(dir.path().join("sp")));
    let mut agent = Agent::new(
        fake.clone(),
        pipe,
        bus,
        AgentConfig::new("m"),
        store.open_writer(&id).unwrap(),
    )
    .unwrap();
    agent.handle().submit("next");
    agent.run_turn().await.unwrap().unwrap();

    let log = store.read(&id).unwrap();
    assert!(log.iter().any(|ev| matches!(&ev.kind, EventKind::ToolResult { call_id, is_error: true, .. } if call_id == "dangling")));
    assert!(log.iter().any(|ev| matches!(&ev.kind, EventKind::TurnEnd { turn_id, reason: TurnEndReason::Failed } if turn_id == "t0")));
    let msgs = &fake.requests()[0].messages;
    let tool_pos = msgs.iter().position(|m| m.role == Role::Tool).unwrap();
    assert_eq!(msgs[tool_pos - 1].tool_calls[0].id, "dangling");
}

#[tokio::test]
async fn interceptors_add_reminders_and_cannot_rewrite_messages() {
    let mut e = env(vec![text("ok")]);
    let _a = e
        .bus
        .intercept::<AgentPreStep, _, _>(0, |mut p| async move {
            p.reminders.push("remember Y".into());
            Verdict::Rewrite(p)
        })
        .unwrap();
    let _b = e
        .bus
        .intercept::<AgentRequest, _, _>(0, |mut r| async move {
            r.request.messages.clear();
            r.request.temperature = Some(0.5);
            Verdict::Rewrite(r)
        })
        .unwrap();
    e.agent.handle().submit("go");
    e.agent.run_turn().await.unwrap().unwrap();
    let req = &e.fake.requests()[0];
    assert_eq!(req.temperature, Some(0.5));
    assert_eq!(req.messages.last().unwrap().content, "remember Y");
    let log = e.store.read(&e.id).unwrap();
    assert!(log.iter().any(|ev| matches!(&ev.kind, EventKind::RequestHeader(h) if h.temperature == Some(0.5) && h.message_count == 2)));
}

#[tokio::test]
async fn pre_step_reject_fails_turn_without_request() {
    let mut e = env(vec![]);
    let _g = e
        .bus
        .intercept::<AgentPreStep, _, _>(0, |_| async { Verdict::Reject("cost cap".into()) })
        .unwrap();
    e.agent.handle().submit("go");
    let s = e.agent.run_turn().await.unwrap().unwrap();
    assert_eq!((s.reason, s.steps), (TurnEndReason::Failed, 0));
    assert!(s.error.unwrap().contains("cost cap"));
    assert!(e.fake.requests().is_empty());
}

#[tokio::test]
async fn system_prompt_logged_once() {
    let mut e = env_with(Fake::new(vec![text("a"), text("b")]), |c| {
        c.system = vec!["sys".into()]
    });
    for m in ["one", "two"] {
        e.agent.handle().submit(m);
        e.agent.run_turn().await.unwrap();
    }
    assert_eq!(
        names(&e).iter().filter(|n| *n == "system/message").count(),
        1
    );
}

/// Answers every approval request as it is announced (after it was logged).
fn answer(e: &Env, approved: bool, reason: Option<&str>) -> Effect {
    let h = e.agent.handle();
    let reason = reason.map(str::to_owned);
    e.bus.on::<ApprovalRequest, _, _>(move |req| {
        let (h, reason) = (h.clone(), reason.clone());
        async move {
            assert!(h.respond(&req.approval_id, approved, reason));
        }
    })
}

fn approvals(e: &Env) -> Vec<(String, Option<bool>)> {
    let log = e.store.read(&e.id).unwrap();
    log.iter()
        .filter_map(|ev| match &ev.kind {
            EventKind::ApprovalRequest { tool, .. } => Some((tool.clone(), None)),
            EventKind::ApprovalResponse { approved, .. } => Some((String::new(), Some(*approved))),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn plan_mode_blocks_write_edit_and_asks_for_bash() {
    let mut e = env_with(
        Fake::new(vec![
            call("c1", "write", "{}"),
            call("c2", "edit", "{}"),
            call("c3", "bash", r#"{"command":"ls"}"#),
            text("done"),
        ]),
        |c| c.mode = Mode::Plan,
    );
    let _a = answer(&e, true, None);
    e.agent.handle().submit("plan it");
    let s = e.agent.run_until_idle().await.unwrap();
    assert_eq!(s[0].reason, TurnEndReason::Completed);

    let r = results(&e);
    for (id, name) in [("c1", "write"), ("c2", "edit")] {
        let (_, content, err) = r.iter().find(|x| x.0 == id).unwrap();
        assert!(
            *err && content.contains("plan mode is read-only"),
            "{name}: {content}"
        );
    }
    assert_eq!(r.iter().find(|x| x.0 == "c3").unwrap().1, "bash ran");
    assert_eq!(*e.ran.lock().unwrap(), ["bash"]);
    // only bash asked; the blocked writers never reached a human
    assert_eq!(
        approvals(&e),
        [("bash".to_owned(), None), (String::new(), Some(true))]
    );
    // writers are not even offered to the model
    let offered: Vec<_> = e.fake.requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert!(offered.contains(&"bash".to_owned()));
    assert!(!offered.contains(&"write".to_owned()) && !offered.contains(&"edit".to_owned()));
    assert_paired(&e);
}

#[tokio::test]
async fn build_mode_asks_only_for_ask_risk_tools() {
    let mut e = env(vec![
        call("c1", "echo", r#"{"text":"a"}"#),
        call("c2", "write", "{}"),
        text("done"),
    ]);
    let _a = answer(&e, true, None);
    e.agent.handle().submit("go");
    e.agent.run_until_idle().await.unwrap();
    assert_eq!(
        approvals(&e),
        [("write".to_owned(), None), (String::new(), Some(true))]
    );
    assert_eq!(*e.ran.lock().unwrap(), ["write"]);
    let offered: Vec<_> = e.fake.requests()[0]
        .tools
        .iter()
        .map(|t| t.name.clone())
        .collect();
    assert!(offered.contains(&"write".to_owned()));
}

#[tokio::test]
async fn approval_round_trip_is_logged_before_the_result() {
    let mut e = env(vec![
        call("c1", "bash", r#"{"command":"echo hi"}"#),
        text("ok"),
    ]);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h = e.agent.handle();
    let s2 = seen.clone();
    let _l = e.bus.on::<ApprovalRequest, _, _>(move |req| {
        let (h, s2) = (h.clone(), s2.clone());
        async move {
            assert_eq!(
                h.pending_approvals(),
                std::slice::from_ref(&req.approval_id)
            );
            s2.lock()
                .unwrap()
                .push((req.tool.clone(), req.detail.clone(), req.risk));
            h.respond(&req.approval_id, true, Some("fine".into()));
        }
    });
    e.agent.handle().submit("go");
    e.agent.run_until_idle().await.unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        [("bash".to_owned(), Some("echo hi".to_owned()), Risk::Ask)]
    );
    assert!(e.agent.handle().pending_approvals().is_empty());

    let n = names(&e);
    let pos = |k: &str| n.iter().position(|x| x == k).unwrap();
    assert!(pos("tool/call") < pos("approval/request"));
    assert!(pos("approval/request") < pos("approval/response"));
    assert!(pos("approval/response") < pos("tool/result"));
    let log = e.store.read(&e.id).unwrap();
    let (rid, pid) = (
        log.iter().find_map(|x| match &x.kind {
            EventKind::ApprovalRequest {
                approval_id,
                call_id,
                ..
            } => {
                assert_eq!(call_id, "c1");
                Some(approval_id.clone())
            }
            _ => None,
        }),
        log.iter().find_map(|x| match &x.kind {
            EventKind::ApprovalResponse {
                approval_id,
                approved,
                reason,
            } => {
                assert!(*approved && reason.as_deref() == Some("fine"));
                Some(approval_id.clone())
            }
            _ => None,
        }),
    );
    assert_eq!(rid, pid);
    assert_eq!(results(&e)[0].1, "bash ran");
    // approvals never leak into the model history
    assert!(
        !derive_messages(&log)
            .iter()
            .any(|m| format!("{m:?}").contains("approval"))
    );
}

#[tokio::test]
async fn denied_call_returns_error_result_and_never_runs() {
    let mut e = env(vec![call("c1", "write", "{}"), text("ack")]);
    let _a = answer(&e, false, Some("not now"));
    e.agent.handle().submit("go");
    let s = e.agent.run_until_idle().await.unwrap();
    assert_eq!(s[0].reason, TurnEndReason::Completed);
    assert!(e.ran.lock().unwrap().is_empty());
    let r = results(&e);
    assert!(
        r[0].2 && r[0].1.contains("denied by user: not now"),
        "{:?}",
        r[0]
    );
    assert_eq!(
        approvals(&e),
        [("write".to_owned(), None), (String::new(), Some(false))]
    );
    // the model is told, and the loop continues to a final answer
    assert_eq!(e.fake.requests().len(), 2);
    assert_paired(&e);
}

#[tokio::test]
async fn cancel_while_parked_closes_the_approval() {
    let mut e = env(vec![call("c1", "bash", "{}")]);
    let h = e.agent.handle();
    let _l = e.bus.on::<ApprovalRequest, _, _>(move |_| {
        let h = h.clone();
        async move { h.cancel() }
    });
    e.agent.handle().submit("go");
    let s = e.agent.run_until_idle().await.unwrap();
    assert_eq!(s[0].reason, TurnEndReason::Cancelled);
    assert!(e.ran.lock().unwrap().is_empty());
    assert_eq!(
        approvals(&e),
        [("bash".to_owned(), None), (String::new(), Some(false))]
    );
    assert!(e.agent.handle().pending_approvals().is_empty());
    assert_paired(&e);
}

#[tokio::test]
async fn respond_to_unknown_id_is_false() {
    let e = env(vec![]);
    assert!(!e.agent.handle().respond("appr_nope", true, None));
}

/// P2.8 subagents and P2.9 compaction + guards.
mod p2 {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use hk_llm::ModelCost;
    use hk_session::ParentRef;

    use super::*;
    use crate::compaction::{COMPACT_PROMPT, SUMMARY_PREFIX};
    use crate::{ChildApprovals, CompactionConfig, SubagentRunner, TaskTool};

    /// Counts overlapping executions; `peak` is the observed fan-out.
    struct Gauge {
        cur: AtomicUsize,
        peak: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Tool for Gauge {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "gauge".into(),
                description: "gauge".into(),
                parameters: json!({"type":"object"}),
            }
        }
        async fn execute(&self, _: ToolCtx, _: Value) -> Result<ToolOutput, ToolError> {
            let n = self.cur.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(30)).await;
            self.cur.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolOutput::ok("gauged"))
        }
    }

    struct Sub {
        agent: Agent,
        fake: Arc<Fake>,
        store: SessionStore,
        id: String,
        peak: Arc<AtomicUsize>,
        ran: Arc<Mutex<Vec<String>>>,
        _dir: tempfile::TempDir,
    }

    fn sub_env(fake: Arc<Fake>, parallel: usize, approvals: ChildApprovals) -> Sub {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path().join("s")).unwrap();
        let writer = store.create(NewSession::default()).unwrap();
        let id = writer.session_id().to_owned();
        let reg = ToolRegistry::new();
        let peak = Arc::new(AtomicUsize::new(0));
        let ran = Arc::new(Mutex::new(Vec::new()));
        std::mem::forget(reg.register(Arc::new(Echo)).unwrap());
        std::mem::forget(
            reg.register(Arc::new(Gauge {
                cur: AtomicUsize::new(0),
                peak: peak.clone(),
            }))
            .unwrap(),
        );
        std::mem::forget(
            reg.register(Arc::new(Gated {
                name: "write",
                ran: ran.clone(),
            }))
            .unwrap(),
        );
        let limits = OutputLimits::new(dir.path().join("spill"));
        let runner = SubagentRunner::new(
            fake.clone(),
            store.clone(),
            reg.clone(),
            limits.clone(),
            AgentConfig::new("child-m"),
        )
        .max_parallel(parallel)
        .approvals(approvals);
        std::mem::forget(
            reg.register(Arc::new(TaskTool::new(Arc::new(runner))))
                .unwrap(),
        );
        let bus = EventBus::new();
        let pipe = ToolPipeline::new(reg, bus.clone(), limits);
        let agent = Agent::new(fake.clone(), pipe, bus, AgentConfig::new("m"), writer).unwrap();
        Sub {
            agent,
            fake,
            store,
            id,
            peak,
            ran,
            _dir: dir,
        }
    }

    fn task_call(id: &str, prompt: &str) -> StreamEvent {
        StreamEvent::ToolCallDelta {
            index: u32::from(id == "t2"),
            id: Some(id.into()),
            name: Some("task".into()),
            arguments_delta: json!({"prompt": prompt, "description": "job"}).to_string(),
        }
    }

    fn two_tasks() -> Script {
        vec![
            Ok(task_call("t1", "one")),
            Ok(task_call("t2", "two")),
            Ok(StreamEvent::Finish(FinishReason::ToolCalls)),
        ]
    }

    fn tool_results(log: &[hk_session::SessionEvent]) -> Vec<(String, String, bool)> {
        log.iter()
            .filter_map(|ev| match &ev.kind {
                EventKind::ToolResult {
                    call_id,
                    content,
                    is_error,
                } => Some((call_id.clone(), content.clone(), *is_error)),
                _ => None,
            })
            .collect()
    }

    fn child_id(result: &str) -> String {
        result
            .rsplit("[subagent session ")
            .next()
            .unwrap()
            .trim_end_matches(']')
            .to_owned()
    }

    #[tokio::test]
    async fn parent_receives_child_result_and_child_log_links_parent() {
        let fake = Fake::new(vec![
            vec![
                Ok(task_call("t1", "find x")),
                Ok(StreamEvent::Finish(FinishReason::ToolCalls)),
            ],
            text("found x"),
            text("thanks"),
        ]);
        let mut s = sub_env(fake, 2, ChildApprovals::Deny);
        s.agent.handle().submit("delegate");
        let sum = s.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(sum.reason, TurnEndReason::Completed);

        let log = s.store.read(&s.id).unwrap();
        let res = tool_results(&log);
        assert_eq!(res.len(), 1);
        assert!(!res[0].2, "{res:?}");
        assert!(res[0].1.starts_with("found x"), "{res:?}");

        // child session: separate log, parent ref points at the spawning tool/call
        let cid = child_id(&res[0].1);
        assert_ne!(cid, s.id);
        let child = s.store.read(&cid).unwrap();
        let call_seq = log
            .iter()
            .find(|e| matches!(e.kind, EventKind::ToolCall { .. }))
            .unwrap()
            .seq;
        match &child[0].kind {
            EventKind::SessionStart { parent, title, .. } => {
                assert_eq!(
                    parent,
                    &Some(ParentRef {
                        session_id: s.id.clone(),
                        seq: call_seq
                    })
                );
                assert_eq!(title.as_deref(), Some("job"));
            }
            k => panic!("first child event: {k:?}"),
        }
        // child saw only its prompt, never the parent history, and has no `task` tool
        let reqs = s.fake.requests();
        assert_eq!(reqs[1].model, "child-m");
        assert_eq!(derive_messages(&child[..3]), reqs[1].messages);
        assert_eq!(reqs[1].messages.len(), 1);
        assert!(reqs[1].tools.iter().all(|t| t.name != "task"));
        assert!(reqs[0].tools.iter().any(|t| t.name == "task"));
        // parent log carries the answer, not the child's messages
        assert!(
            !log.iter().any(
                |e| matches!(&e.kind, EventKind::UserMessage { content, .. } if content == "find x")
            )
        );
        // parent's second request is replayable from its own log
        assert_eq!(reqs[2].messages.last().unwrap().role, Role::Tool);
    }

    async fn peak_for(
        parallel: usize,
        scripts: Vec<Script>,
    ) -> (usize, Vec<(String, String, bool)>) {
        let mut s = sub_env(Fake::new(scripts), parallel, ChildApprovals::Deny);
        s.agent.handle().submit("fan out");
        let sum = s.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(sum.reason, TurnEndReason::Completed);
        let res = tool_results(&s.store.read(&s.id).unwrap());
        (s.peak.load(Ordering::SeqCst), res)
    }

    #[tokio::test]
    async fn parallel_limit_bounds_concurrent_children() {
        // Scripts are consumed in request order across all agents; with two slots both
        // children reach `gauge` before either asks for its second reply.
        let (peak2, res) = peak_for(
            2,
            vec![
                two_tasks(),
                call("g1", "gauge", "{}"),
                call("g2", "gauge", "{}"),
                text("A"),
                text("B"),
                text("done"),
            ],
        )
        .await;
        assert_eq!(peak2, 2);
        assert_eq!(res.len(), 2);
        assert!(
            res[0].1.starts_with('A') && res[1].1.starts_with('B'),
            "{res:?}"
        );

        // One slot: the second child starts only after the first finished.
        let (peak1, res) = peak_for(
            1,
            vec![
                two_tasks(),
                call("g1", "gauge", "{}"),
                text("A"),
                call("g2", "gauge", "{}"),
                text("B"),
                text("done"),
            ],
        )
        .await;
        assert_eq!(peak1, 1);
        assert!(
            res[0].1.starts_with('A') && res[1].1.starts_with('B'),
            "{res:?}"
        );
    }

    #[tokio::test]
    async fn failed_child_becomes_error_result_and_approvals_are_denied() {
        let fake = Fake::new(vec![
            vec![
                Ok(task_call("t1", "write it")),
                Ok(StreamEvent::Finish(FinishReason::ToolCalls)),
            ],
            // child asks to write: nobody can approve, so it is denied, not parked
            call("w1", "write", "{}"),
            vec![Err(LlmError::Protocol("boom".into()))],
            text("ok"),
        ]);
        let mut s = sub_env(fake, 2, ChildApprovals::Deny);
        s.agent.handle().submit("go");
        let sum = s.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(sum.reason, TurnEndReason::Completed);
        assert!(s.ran.lock().unwrap().is_empty());

        let res = tool_results(&s.store.read(&s.id).unwrap());
        assert!(res[0].2 && res[0].1.contains("Failed"), "{res:?}");
        let child = s.store.read(&child_id(&res[0].1)).unwrap();
        let child_res = tool_results(&child);
        assert!(
            child_res[0].2 && child_res[0].1.contains("denied"),
            "{child_res:?}"
        );
        assert!(child.iter().any(|e| matches!(
            &e.kind,
            EventKind::ApprovalResponse {
                approved: false,
                ..
            }
        )));
    }

    // ---- compaction ----

    fn cfg_compact(c: &mut AgentConfig, keep: usize) {
        c.system = vec!["sys".into()];
        c.compaction = Some(CompactionConfig {
            threshold_tokens: 1,
            keep_recent_steps: keep,
        });
    }

    fn header_index(log: &[hk_session::SessionEvent], nth: usize) -> usize {
        log.iter()
            .enumerate()
            .filter(|(_, e)| matches!(e.kind, EventKind::RequestHeader(_)))
            .nth(nth)
            .unwrap()
            .0
    }

    #[tokio::test]
    async fn compaction_replaces_old_history_and_keeps_log_replayable() {
        let mut e = env_with(
            Fake::new(vec![
                call("a", "echo", r#"{"text":"a"}"#),
                call("b", "echo", r#"{"text":"b"}"#),
                call("c", "echo", r#"{"text":"c"}"#),
                text("SUMMARY-ONE"),
                text("final"),
            ]),
            |c| cfg_compact(c, 2),
        );
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!((s.reason, s.steps), (TurnEndReason::Completed, 4));
        assert_paired(&e);

        let log = e.store.read(&e.id).unwrap();
        let comp: Vec<_> = log
            .iter()
            .filter_map(|ev| match &ev.kind {
                EventKind::CompactionSummary {
                    summary,
                    through_seq,
                } => Some((ev.seq, summary.clone(), *through_seq)),
                _ => None,
            })
            .collect();
        assert_eq!(comp.len(), 1);
        assert!(comp[0].1.starts_with(SUMMARY_PREFIX) && comp[0].1.contains("SUMMARY-ONE"));
        assert!(comp[0].2 < comp[0].0);

        let reqs = e.fake.requests();
        // summary request = derive(head) + the fixed prompt: sys, user, asst(a), tool(a), prompt
        assert_eq!(reqs[3].messages.len(), 5);
        assert_eq!(reqs[3].messages[4].content, COMPACT_PROMPT);
        assert!(reqs[3].tools.is_empty());
        // the next real request is exactly derive(log before its header)
        let h = header_index(&log, 3);
        assert_eq!(reqs[4].messages, derive_messages(&log[..h]));
        let m = &reqs[4].messages;
        assert_eq!(m[0].role, Role::System);
        assert!(m[1].content.contains("SUMMARY-ONE") && m[1].role == Role::User);
        let all: String = m.iter().map(|x| x.content.clone()).collect();
        assert!(!all.contains("echo:a"), "compacted result still visible");
        assert!(all.contains("echo:b") && all.contains("echo:c"));
        // reopening the log yields the same model history
        assert_eq!(derive_messages(&log), derive_messages(e.agent.events()));
    }

    #[tokio::test]
    async fn second_compaction_folds_the_first_summary_in() {
        let mut e = env_with(
            Fake::new(vec![
                call("a", "echo", r#"{"text":"a"}"#),
                call("b", "echo", r#"{"text":"b"}"#),
                text("SUM1"),
                call("c", "echo", r#"{"text":"c"}"#),
                call("d", "echo", r#"{"text":"d"}"#),
                text("SUM2"),
                text("final"),
            ]),
            |c| cfg_compact(c, 1),
        );
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(s.reason, TurnEndReason::Completed);
        assert_paired(&e);

        let reqs = e.fake.requests();
        // second summary request saw the first summary, not the raw history it replaced
        let sum2 = &reqs[5].messages;
        assert_eq!(sum2.last().unwrap().content, COMPACT_PROMPT);
        assert!(sum2.iter().any(|m| m.content.contains("SUM1")));
        assert!(!sum2.iter().any(|m| m.content.contains("echo:a")));

        let last = &reqs.last().unwrap().messages;
        let summaries: Vec<_> = last
            .iter()
            .filter(|m| m.content.starts_with(SUMMARY_PREFIX))
            .collect();
        assert_eq!(summaries.len(), 1, "{last:?}");
        assert!(summaries[0].content.contains("SUM2"));
        assert!(!last.iter().any(|m| m.content.contains("SUM1")));
        let log = e.store.read(&e.id).unwrap();
        let h = header_index(&log, 4);
        assert_eq!(*last, derive_messages(&log[..h]));
    }

    #[tokio::test]
    async fn failed_summary_leaves_history_intact() {
        let mut e = env_with(
            Fake::new(vec![
                call("a", "echo", r#"{"text":"a"}"#),
                call("b", "echo", r#"{"text":"b"}"#),
                vec![Err(LlmError::Protocol("no summary".into()))],
                text("final"),
            ]),
            |c| cfg_compact(c, 1),
        );
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(s.reason, TurnEndReason::Completed);
        let log = e.store.read(&e.id).unwrap();
        assert!(
            !log.iter()
                .any(|ev| matches!(ev.kind, EventKind::CompactionSummary { .. }))
        );
        assert!(log.iter().any(|ev| matches!(
            &ev.kind,
            EventKind::AssistantAttempt { error: Some(m), .. } if m.contains("compaction failed")
        )));
        assert_paired(&e);
    }

    #[tokio::test]
    async fn manual_compact_across_turns() {
        let mut e = env_with(
            Fake::new(vec![text("r1"), text("r2"), text("SUM"), text("r3")]),
            |c| c.system = vec!["sys".into()],
        );
        let h = e.agent.handle();
        for m in ["u1", "u2"] {
            h.submit(m);
            e.agent.run_until_idle().await.unwrap();
        }
        // default keep (4 steps) is longer than this log: nothing to cut
        assert!(!e.agent.compact().await.unwrap());
        e.agent.set_config({
            let mut c = AgentConfig::new("m");
            c.compaction = Some(CompactionConfig {
                threshold_tokens: u64::MAX,
                keep_recent_steps: 1,
            });
            c
        });
        assert!(e.agent.compact().await.unwrap());
        h.submit("u3");
        e.agent.run_until_idle().await.unwrap();
        let reqs = e.fake.requests();
        let last = &reqs.last().unwrap().messages;
        let roles: Vec<_> = last.iter().map(|m| (m.role, m.content.as_str())).collect();
        assert!(
            roles[0].0 == Role::System && roles[1].1.contains("SUM"),
            "{roles:?}"
        );
        assert!(!roles.iter().any(|(_, c)| *c == "u1"));
        assert!(roles.iter().any(|(_, c)| *c == "u2") && roles.iter().any(|(_, c)| *c == "u3"));
    }

    // ---- guards ----

    #[tokio::test]
    async fn repeated_identical_call_gets_reminder_then_aborts() {
        let same = |i: usize| call(&format!("x{i}"), "echo", r#"{"text":"same"}"#);
        let mut e = env_with(
            Fake::new(vec![same(0), same(1), same(2), text("fine")]),
            |_| {},
        );
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!((s.reason, s.steps), (TurnEndReason::Completed, 4));
        let log = e.store.read(&e.id).unwrap();
        let reminder = log.iter().position(|ev| {
            matches!(&ev.kind, EventKind::UserMessage { content, .. } if content.contains("identical arguments"))
        });
        // logged before the 4th request, which therefore carries it
        assert!(reminder.is_some());
        assert!(
            e.fake.requests()[3]
                .messages
                .iter()
                .any(|m| m.content.contains("identical arguments"))
        );

        let mut e = env_with(Fake::new((0..8).map(same).collect()), |c| {
            c.guards.repeat_remind_after = 2;
            c.guards.repeat_abort_after = 4;
        });
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!((s.reason, s.steps), (TurnEndReason::Failed, 4));
        assert!(s.error.unwrap().contains("repeat guard"));
        assert_eq!(e.fake.requests().len(), 4);
        assert_paired(&e);
    }

    #[tokio::test]
    async fn step_timeout_fails_a_hung_request() {
        let mut e = env_with(Fake::with_hang(vec![(Vec::new(), true)]), |c| {
            c.guards.step_timeout = Some(Duration::from_millis(50));
        });
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(s.reason, TurnEndReason::Failed);
        assert!(s.error.unwrap().contains("step timeout exceeded"));
        assert_eq!(names(&e).last().unwrap(), "turn/end:Failed");
    }

    #[tokio::test]
    async fn tool_timeout_errors_the_call_and_the_turn_goes_on() {
        let mut e = env_with(
            Fake::new(vec![call("s", "slow", "{}"), text("recovered")]),
            |c| c.guards.tool_timeout = Some(Duration::from_millis(50)),
        );
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!((s.reason, s.steps), (TurnEndReason::Completed, 2));
        let r = results(&e);
        assert!(r[0].2 && r[0].1.contains("timed out"), "{r:?}");
        assert_paired(&e);
    }

    #[tokio::test]
    async fn turn_timeout_aborts_running_tool_and_fails_turn() {
        let mut e = env_with(Fake::new(vec![call("s", "slow", "{}")]), |c| {
            c.guards.turn_timeout = Some(Duration::from_millis(80));
        });
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(s.reason, TurnEndReason::Failed);
        assert!(s.error.unwrap().contains("turn timeout"));
        assert_paired(&e);
    }

    #[tokio::test]
    async fn cost_cap_stops_a_continuing_turn_but_not_a_finished_one() {
        let usage = Usage {
            input_tokens: 1000,
            ..Default::default()
        };
        let price = ModelCost {
            input: 1000.0, // $1 for 1000 tokens
            ..Default::default()
        };
        let cap = |c: &mut AgentConfig| {
            c.guards.max_cost_usd = Some(0.5);
            c.guards.price = Some(price);
        };
        let mut with_call = call("c", "echo", r#"{"text":"a"}"#);
        with_call.insert(1, Ok(StreamEvent::Usage(usage)));
        let mut e = env_with(Fake::new(vec![with_call, text("never")]), cap);
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!((s.reason, s.steps), (TurnEndReason::Failed, 1));
        assert!(s.error.unwrap().contains("cost cap exceeded"));
        assert_eq!(e.fake.requests().len(), 1);
        assert_paired(&e);

        let mut done = text("answer");
        done.insert(1, Ok(StreamEvent::Usage(usage)));
        let mut e = env_with(Fake::new(vec![done]), cap);
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(s.reason, TurnEndReason::Completed);
    }
}

mod jobs {
    use hk_core::{JobEvent, JobInfo, JobKind, JobRegistry, JobState};

    use super::*;
    use crate::{SubagentRunner, TaskTool};

    fn by_kind(reg: &JobRegistry, kind: &JobKind) -> Vec<JobInfo> {
        reg.list().into_iter().filter(|j| &j.kind == kind).collect()
    }

    #[tokio::test]
    async fn jobs_nest_turn_step_tool() {
        let mut e = env(vec![call("e1", "echo", r#"{"text":"x"}"#), text("done")]);
        let reg = JobRegistry::new();
        e.agent.set_jobs(reg.clone(), None);
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(s.reason, TurnEndReason::Completed);

        let turn = &by_kind(&reg, &JobKind::AgentTurn)[0];
        assert_eq!((turn.parent, &turn.state), (None, &JobState::Finished));
        let steps = by_kind(&reg, &JobKind::Step);
        assert_eq!(steps.len(), 2);
        assert!(
            steps
                .iter()
                .all(|s| s.parent == Some(turn.id) && s.state == JobState::Finished)
        );
        let tool = &by_kind(&reg, &JobKind::Tool)[0];
        assert_eq!(tool.parent, Some(steps[0].id));
        let key = format!("tool:{}:e1", e.id);
        assert_eq!(tool.key.as_deref(), Some(key.as_str()));
        assert_eq!(tool.state, JobState::Finished);
    }

    #[tokio::test]
    async fn jobs_cancel_turn_job_cancels_turn_and_subtree() {
        let mut e = env(vec![call("s1", "slow", "{}")]);
        let reg = JobRegistry::new();
        e.agent.set_jobs(reg.clone(), None);
        let mut rx = reg.subscribe();
        let r = reg.clone();
        let watcher = tokio::spawn(async move {
            let mut seen = Vec::new();
            while let Ok(ev) = rx.recv().await {
                if let JobEvent::Started { job } = &ev
                    && job.kind == JobKind::Tool
                {
                    // The turn job is the first one started.
                    assert!(r.cancel(hk_core::JobId(1)));
                }
                let done = matches!(&ev, JobEvent::Cancelled { id } if id.0 == 1);
                seen.push(ev);
                if done {
                    break;
                }
            }
            seen
        });
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(s.reason, TurnEndReason::Cancelled);
        assert_paired(&e);

        let seen = watcher.await.unwrap();
        let requested: Vec<u64> = seen
            .iter()
            .filter_map(|ev| match ev {
                JobEvent::CancelRequested { id } => Some(id.0),
                _ => None,
            })
            .collect();
        assert_eq!(requested, vec![1, 2, 3]); // turn, step, tool
        assert!(reg.list().iter().all(|j| j.state == JobState::Cancelled));
    }

    #[tokio::test]
    async fn jobs_skip_tool_keeps_turn_going() {
        let mut e = env(vec![call("s1", "slow", "{}"), text("done")]);
        let reg = JobRegistry::new();
        e.agent.set_jobs(reg.clone(), None);
        let mut rx = reg.subscribe();
        let r = reg.clone();
        tokio::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                if let JobEvent::Started { job } = &ev
                    && job.kind == JobKind::Tool
                {
                    r.cancel(job.id);
                    break;
                }
            }
        });
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(s.reason, TurnEndReason::Completed);
        assert_paired(&e);
        assert_eq!(by_kind(&reg, &JobKind::Tool)[0].state, JobState::Cancelled);
    }

    #[tokio::test]
    async fn pause_after_step_ends_turn_cleanly() {
        let mut e = env(vec![call("e1", "echo", r#"{"text":"x"}"#), text("done")]);
        e.agent.handle().pause_after_step();
        e.agent.handle().submit("go");
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(s.reason, TurnEndReason::Paused);
        assert_eq!(s.steps, 1);
        assert_paired(&e);
    }

    #[tokio::test]
    async fn jobs_handle_cancel_goes_through_job_tree() {
        let partial = vec![Ok(StreamEvent::TextDelta { text: "p".into() })];
        let mut e = env_with(Fake::with_hang(vec![(partial, true)]), |_| {});
        let reg = JobRegistry::new();
        e.agent.set_jobs(reg.clone(), None);
        let mut rx = reg.subscribe();
        let h = e.agent.handle();
        h.submit("go");
        cancel_soon(h);
        let s = e.agent.run_turn().await.unwrap().unwrap();
        assert_eq!(s.reason, TurnEndReason::Cancelled);
        let mut evs = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            evs.push(ev.name());
        }
        let req = evs
            .iter()
            .position(|n| *n == "job/cancel-requested")
            .unwrap();
        let end = evs.iter().rposition(|n| *n == "job/cancelled").unwrap();
        assert!(req < end);
    }

    #[tokio::test]
    async fn jobs_subagent_turn_nests_under_task_call() {
        let task = vec![
            Ok(StreamEvent::ToolCallDelta {
                index: 0,
                id: Some("t1".into()),
                name: Some("task".into()),
                arguments_delta: json!({"prompt": "p", "description": "sub job"}).to_string(),
            }),
            Ok(StreamEvent::Finish(FinishReason::ToolCalls)),
        ];
        let fake = Fake::new(vec![task, text("child answer"), text("parent done")]);
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path().join("s")).unwrap();
        let writer = store.create(NewSession::default()).unwrap();
        let tools = ToolRegistry::new();
        std::mem::forget(tools.register(Arc::new(Echo)).unwrap());
        let limits = OutputLimits::new(dir.path().join("spill"));
        let jobs = JobRegistry::new();
        let runner = SubagentRunner::new(
            fake.clone(),
            store.clone(),
            tools.clone(),
            limits.clone(),
            AgentConfig::new("child-m"),
        )
        .jobs(jobs.clone());
        std::mem::forget(
            tools
                .register(Arc::new(TaskTool::new(Arc::new(runner))))
                .unwrap(),
        );
        let bus = EventBus::new();
        let pipe = ToolPipeline::new(tools, bus.clone(), limits);
        let mut agent = Agent::new(fake.clone(), pipe, bus, AgentConfig::new("m"), writer).unwrap();
        agent.set_jobs(jobs.clone(), None);
        agent.handle().submit("go");
        let s = agent.run_turn().await.unwrap().unwrap();
        assert_eq!(s.reason, TurnEndReason::Completed);

        let tool = &by_kind(&jobs, &JobKind::Tool)[0];
        let sub = &by_kind(&jobs, &JobKind::Subagent)[0];
        assert_eq!(sub.parent, Some(tool.id));
        assert_eq!(sub.title, "sub job");
        let turns = by_kind(&jobs, &JobKind::AgentTurn);
        assert_eq!(turns.len(), 2);
        assert!(turns.iter().any(|t| t.parent == Some(sub.id)));
        assert!(jobs.list().iter().all(|j| j.state == JobState::Finished));
    }
}
