//! State-machine tests against a scripted fake provider (no network).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use hk_core::{EventBus, Verdict};
use hk_llm::{
    CancellationToken, ChatRequest, EventStream, FinishReason, LlmError, ModelInfo, Provider, Role,
    StreamEvent, Usage,
};
use hk_session::{EventKind, NewSession, SessionStore, TurnEndReason, derive_messages};
use hk_tools::{
    OutputLimits, Tool, ToolCtx, ToolError, ToolOutput, ToolPipeline, ToolRegistry, ToolSpec,
};
use serde_json::{Value, json};

use crate::{Agent, AgentConfig, AgentHandle, AgentPreStep, AgentRequest};

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

struct Env {
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
    assert_eq!(reqs[1].tools[0].name, "echo");
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
        EventKind::UserMessage {
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
