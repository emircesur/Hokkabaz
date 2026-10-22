//! End-to-end against an in-process fake ACP agent over `tokio::io::duplex`:
//! initialize -> session/new -> prompt -> chunks -> permission -> fs -> cancel.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hk_acp::{
    AcpEngine, ApprovalPrompt, Approver, Decision, EngineEvent, EngineOptions, SessionMapper,
    StopReason,
};
use hk_session::{EventKind, NewSession, SessionStore, TurnEndReason, derive_messages};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

type Seen = Arc<Mutex<Vec<Value>>>;

struct Agent {
    r: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    w: WriteHalf<DuplexStream>,
    seen: Seen,
    root: std::path::PathBuf,
}

impl Agent {
    async fn send(&mut self, v: Value) {
        let mut s = v.to_string();
        s.push('\n');
        self.w.write_all(s.as_bytes()).await.unwrap();
    }

    async fn next(&mut self) -> Option<Value> {
        let l = self.r.next_line().await.unwrap()?;
        Some(serde_json::from_str(&l).unwrap())
    }

    async fn update(&mut self, u: Value) {
        self.send(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":u}}))
            .await;
    }

    /// Send a request and wait for its response, recording any `session/cancel` seen meanwhile.
    async fn ask(&mut self, id: u64, method: &str, params: Value) -> (Value, bool) {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        let mut cancelled = false;
        loop {
            let m = self.next().await.expect("client hung up");
            if m.get("method").and_then(Value::as_str) == Some("session/cancel") {
                cancelled = true;
            }
            if m.get("id") == Some(&json!(id)) && m.get("method").is_none() {
                return (m, cancelled);
            }
        }
    }

    async fn run(mut self) {
        while let Some(m) = self.next().await {
            let id = m.get("id").cloned();
            match m["method"].as_str() {
                Some("initialize") => {
                    assert_eq!(m["params"]["protocolVersion"], 1);
                    assert_eq!(
                        m["params"]["clientCapabilities"]["fs"]["readTextFile"],
                        true
                    );
                    assert_eq!(m["params"]["clientCapabilities"]["terminal"], false);
                    self.reply(
                        id,
                        json!({"protocolVersion":1,"agentCapabilities":{},"authMethods":[],
                        "agentInfo":{"name":"fake","version":"0"}}),
                    )
                    .await;
                }
                Some("session/new") => {
                    assert_eq!(m["params"]["cwd"], self.root.to_str().unwrap());
                    self.reply(id, json!({"sessionId":"sess-1"})).await;
                }
                Some("session/prompt") => {
                    let text = m["params"]["prompt"][0]["text"]
                        .as_str()
                        .unwrap()
                        .to_owned();
                    let stop = if text == "wait" {
                        self.wait_turn().await
                    } else {
                        self.full_turn().await
                    };
                    self.reply(id, json!({"stopReason": stop})).await;
                }
                _ => {}
            }
        }
    }

    async fn reply(&mut self, id: Option<Value>, result: Value) {
        self.send(json!({"jsonrpc":"2.0","id":id,"result":result}))
            .await;
    }

    async fn full_turn(&mut self) -> &'static str {
        self.update(
            json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hmm"}}),
        )
        .await;
        self.update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Reading "}})).await;
        self.update(
            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"file."}}),
        )
        .await;
        self.update(json!({"sessionUpdate":"plan","entries":[{"content":"read","priority":"high","status":"pending"}]})).await;
        self.update(
            json!({"sessionUpdate":"tool_call","toolCallId":"c1","title":"Edit a.txt","kind":"edit",
            "status":"pending","rawInput":{"path":"a.txt"}}),
        )
        .await;

        let (resp, _) = self
            .ask(
                100,
                "session/request_permission",
                json!({"sessionId":"sess-1","toolCall":{"toolCallId":"c1","title":"Edit a.txt","rawInput":{"path":"a.txt"}},
                    "options":[{"optionId":"yes","name":"Allow","kind":"allow_once"},
                               {"optionId":"no","name":"Reject","kind":"reject_once"}]}),
            )
            .await;
        self.seen.lock().unwrap().push(resp["result"].clone());

        let file = self.root.join("a.txt");
        let (w, _) = self
            .ask(
                101,
                "fs/write_text_file",
                json!({"sessionId":"sess-1","path":file,"content":"l1\nl2\n"}),
            )
            .await;
        self.seen.lock().unwrap().push(w);
        let (r, _) = self
            .ask(
                102,
                "fs/read_text_file",
                json!({"sessionId":"sess-1","path":file,"line":2}),
            )
            .await;
        self.seen.lock().unwrap().push(r);
        let (esc, _) = self
            .ask(
                103,
                "fs/read_text_file",
                json!({"sessionId":"sess-1","path":"/etc/passwd"}),
            )
            .await;
        self.seen.lock().unwrap().push(esc);
        let (term, _) = self
            .ask(
                104,
                "terminal/create",
                json!({"sessionId":"sess-1","command":"ls"}),
            )
            .await;
        self.seen.lock().unwrap().push(term);

        self.update(
            json!({"sessionUpdate":"tool_call_update","toolCallId":"c1","status":"completed",
            "content":[{"type":"content","content":{"type":"text","text":"edited"}}]}),
        )
        .await;
        self.update(
            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Done."}}),
        )
        .await;
        "end_turn"
    }

    async fn wait_turn(&mut self) -> &'static str {
        self.update(json!({"sessionUpdate":"tool_call","toolCallId":"c9","title":"Run","kind":"execute","status":"pending"})).await;
        let (resp, cancelled) = self
            .ask(
                200,
                "session/request_permission",
                json!({"sessionId":"sess-1","toolCall":{"toolCallId":"c9"},
                    "options":[{"optionId":"yes","name":"Allow","kind":"allow_once"}]}),
            )
            .await;
        self.seen.lock().unwrap().push(resp["result"].clone());
        // Cancel notification may land before or after the permission response.
        let mut got = cancelled;
        while !got {
            let m = self.next().await.expect("client hung up");
            got = m["method"] == "session/cancel";
        }
        "cancelled"
    }
}

struct Scripted(Decision);

#[async_trait]
impl Approver for Scripted {
    async fn decide(&self, _: &ApprovalPrompt) -> Decision {
        self.0
    }
}

/// Never answers: models a user staring at the prompt while cancel arrives.
struct Hang;

#[async_trait]
impl Approver for Hang {
    async fn decide(&self, _: &ApprovalPrompt) -> Decision {
        std::future::pending().await
    }
}

async fn start(approver: Arc<dyn Approver>) -> (AcpEngine, Seen, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (c, a) = tokio::io::duplex(1 << 16);
    let (cr, cw) = tokio::io::split(c);
    let (ar, aw) = tokio::io::split(a);
    let seen = Seen::default();
    tokio::spawn(
        Agent {
            r: BufReader::new(ar).lines(),
            w: aw,
            seen: seen.clone(),
            root: root.clone(),
        }
        .run(),
    );
    let engine = AcpEngine::connect(cr, cw, EngineOptions::new(root, approver))
        .await
        .unwrap();
    (engine, seen, dir)
}

#[tokio::test]
async fn full_turn_maps_to_session_log() {
    let (mut engine, seen, dir) = start(Arc::new(Scripted(Decision::AllowOnce))).await;
    assert_eq!(engine.session_id(), "sess-1");
    assert_eq!(engine.agent_info().unwrap().name, "fake");
    let mut rx = engine.take_events().unwrap();

    let stop = engine.prompt("hello").await.unwrap();
    assert_eq!(stop, StopReason::EndTurn);

    let mut evs = Vec::new();
    while let Ok(e) = rx.try_recv() {
        evs.push(e);
    }
    assert!(
        evs.iter()
            .any(|e| matches!(e, EngineEvent::Plan(p) if p.len() == 1))
    );
    assert!(
        evs.iter()
            .any(|e| matches!(e, EngineEvent::ApprovalRequested(p) if p.call_id == "c1"))
    );
    assert!(
        evs.iter()
            .any(|e| matches!(e, EngineEvent::ApprovalResolved { approved: true, .. }))
    );

    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen[0]["outcome"],
        json!({"outcome":"selected","optionId":"yes"})
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "l1\nl2\n"
    );
    assert_eq!(seen[2]["result"]["content"], "l2\n");
    assert!(
        seen[3]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outside workspace")
    );
    assert_eq!(seen[4]["error"]["code"], -32601);

    // Map into a real session log and replay it.
    let store_dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(store_dir.path()).unwrap();
    let mut w = store.create(NewSession::default()).unwrap();
    let mut m = SessionMapper::new("turn_0");
    let mut kinds = m.begin("hello");
    for e in &evs {
        kinds.extend(m.push(e));
    }
    kinds.extend(m.finish(stop));
    for k in kinds {
        w.append(k).unwrap();
    }
    let log = store.read(w.session_id()).unwrap();
    assert!(matches!(
        log.last().unwrap().kind,
        EventKind::TurnEnd {
            reason: TurnEndReason::Completed,
            ..
        }
    ));
    let msgs = derive_messages(&log);
    let dump = serde_json::to_string(&msgs).unwrap();
    for needle in ["hello", "Reading file.", "Edit a.txt", "edited", "Done."] {
        assert!(dump.contains(needle), "{needle} missing from {dump}");
    }
    engine.shutdown().await;
}

#[tokio::test]
async fn denied_permission_selects_reject_option() {
    let (mut engine, seen, _dir) = start(Arc::new(Scripted(Decision::Deny))).await;
    let mut rx = engine.take_events().unwrap();
    engine.prompt("hello").await.unwrap();
    assert_eq!(
        seen.lock().unwrap()[0]["outcome"],
        json!({"outcome":"selected","optionId":"no"})
    );
    let mut denied = false;
    while let Ok(e) = rx.try_recv() {
        denied |= matches!(
            e,
            EngineEvent::ApprovalResolved {
                approved: false,
                ..
            }
        );
    }
    assert!(denied);
}

#[tokio::test]
async fn cancel_resolves_parked_permission_and_prompt() {
    let (mut engine, seen, _dir) = start(Arc::new(Hang)).await;
    let mut rx = engine.take_events().unwrap();
    let engine = Arc::new(engine);
    let e2 = engine.clone();
    let turn = tokio::spawn(async move { e2.prompt("wait").await });

    // Wait until the permission prompt is parked, then cancel.
    loop {
        if matches!(rx.recv().await, Some(EngineEvent::ApprovalRequested(_))) {
            break;
        }
    }
    engine.cancel();
    let stop = tokio::time::timeout(std::time::Duration::from_secs(5), turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(stop, StopReason::Cancelled);
    assert_eq!(
        seen.lock().unwrap()[0],
        json!({"outcome":{"outcome":"cancelled"}})
    );
}

#[tokio::test]
async fn agent_hangup_fails_pending_call() {
    let dir = tempfile::tempdir().unwrap();
    let (c, a) = tokio::io::duplex(4096);
    let (cr, cw) = tokio::io::split(c);
    drop(a);
    let err = AcpEngine::connect(
        cr,
        cw,
        EngineOptions::new(dir.path().to_path_buf(), Arc::new(hk_acp::DenyAll)),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, hk_acp::AcpError::Closed), "{err}");
}
