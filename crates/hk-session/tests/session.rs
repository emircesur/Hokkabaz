use std::io::Write;
use std::path::Path;

use hk_llm::{Role, Usage};
use hk_session::*;
use serde_json::{Value, json};

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn store() -> (tempfile::TempDir, SessionStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path().join("sessions")).unwrap();
    (dir, store)
}

fn user(c: &str) -> EventKind {
    EventKind::UserMessage { images: Vec::new(), content: c.into() }
}
fn asst(c: &str) -> EventKind {
    EventKind::AssistantMessage {
        content: c.into(),
        reasoning: None,
    }
}
fn turn_start(id: &str) -> EventKind {
    EventKind::TurnStart { turn_id: id.into() }
}
fn turn_end(id: &str) -> EventKind {
    EventKind::TurnEnd {
        turn_id: id.into(),
        reason: TurnEndReason::Completed,
    }
}

#[test]
fn replay_fixture_yields_exact_messages() {
    let events = read_log(&fixture("basic.jsonl")).unwrap();
    assert_eq!(events.len(), 21);
    let got = serde_json::to_value(derive_messages(&events)).unwrap();
    let want: Value =
        serde_json::from_str(&std::fs::read_to_string(fixture("basic.messages.json")).unwrap())
            .unwrap();
    assert_eq!(got, want);
}

#[test]
fn fixture_roundtrips_through_serde() {
    let text = std::fs::read_to_string(fixture("basic.jsonl")).unwrap();
    for line in text.lines() {
        let ev: SessionEvent = serde_json::from_str(line).unwrap();
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            serde_json::from_str::<Value>(line).unwrap(),
            "{line}"
        );
    }
}

#[test]
fn usage_meter_and_summary_fold() {
    let events = read_log(&fixture("basic.jsonl")).unwrap();
    let m: UsageMeter = fold(&events);
    assert_eq!(m.steps, 2);
    assert_eq!(m.usage.input_tokens, 250);
    assert_eq!(m.usage.cache_read_tokens, 100);
    let mut s = SessionSummary::new("fixture");
    events.iter().for_each(|e| s.apply(e));
    assert_eq!(s.turn_count, 2);
    assert_eq!(s.title.as_deref(), Some("List files"));
    assert_eq!(s.last_seq, 20);
}

#[test]
fn compaction_drops_old_keeps_system() {
    let mk = |seq, kind| SessionEvent {
        seq,
        ts_ms: 0,
        kind,
    };
    let events = vec![
        mk(
            1,
            EventKind::SystemMessage {
                content: "sys".into(),
            },
        ),
        mk(2, user("old q")),
        mk(3, asst("old a")),
        mk(4, user("recent q")),
        mk(
            5,
            EventKind::CompactionSummary {
                summary: "S".into(),
                through_seq: 3,
            },
        ),
        mk(6, asst("new a")),
        mk(
            7,
            EventKind::ToolCall {
                call_id: "c".into(),
                name: "t".into(),
                arguments: json!({}),
            },
        ),
    ];
    let msgs = derive_messages(&events);
    let shape: Vec<(Role, &str)> = msgs.iter().map(|m| (m.role, m.content.as_str())).collect();
    assert_eq!(
        shape,
        vec![
            (Role::System, "sys"),
            (Role::User, "S"),
            (Role::User, "recent q"),
            (Role::Assistant, "new a"),
        ]
    );
    // Call after compaction still lands on the surviving assistant message.
    assert_eq!(msgs[3].tool_calls.len(), 1);
}

#[test]
fn writer_reader_roundtrip_and_index() {
    let (_d, store) = store();
    let mut w = store
        .create(NewSession {
            id: Some("s1".into()),
            cwd: Some("/w".into()),
            model: Some("m".into()),
            title: Some("T".into()),
        })
        .unwrap();
    w.append(turn_start("t1")).unwrap();
    w.append(user("hi")).unwrap();
    w.append(EventKind::StepEnd {
        turn_id: "t1".into(),
        step: 0,
        finish: None,
        usage: Some(Usage {
            input_tokens: 7,
            output_tokens: 3,
            ..Usage::default()
        }),
    })
    .unwrap();
    // Index not refreshed until turn end.
    assert_eq!(store.index().get("s1").unwrap().unwrap().turn_count, 0);
    w.append(turn_end("t1")).unwrap();

    let row = store.index().get("s1").unwrap().unwrap();
    assert_eq!(row.turn_count, 1);
    assert_eq!(row.usage.input_tokens, 7);
    assert_eq!(row.title.as_deref(), Some("T"));
    assert_eq!(row.cwd.as_deref(), Some("/w"));
    assert_eq!(row.event_count, 5);

    let events = store.read("s1").unwrap();
    let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [0, 1, 2, 3, 4]);
    assert_eq!(&row, w.summary());
}

#[test]
fn reopen_continues_seq_and_repairs_torn_tail() {
    let (_d, store) = store();
    let mut w = store
        .create(NewSession {
            id: Some("s".into()),
            ..Default::default()
        })
        .unwrap();
    w.append(user("a")).unwrap();
    w.sync().unwrap();
    let path = w.path().to_owned();
    drop(w);

    // Crash mid-line.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(br#"{"seq":2,"ts_ms":1,"type":"user/mess"#)
        .unwrap();
    assert_eq!(store.read("s").unwrap().len(), 2);

    let mut w = store.open_writer("s").unwrap();
    assert_eq!(w.next_seq(), 2);
    w.append(user("b")).unwrap();
    w.sync().unwrap();
    let events = store.read("s").unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[2].kind, user("b"));
}

#[test]
fn reopen_keeps_valid_line_missing_newline() {
    let (_d, store) = store();
    let mut w = store
        .create(NewSession {
            id: Some("s".into()),
            ..Default::default()
        })
        .unwrap();
    let path = w.path().to_owned();
    w.sync().unwrap();
    drop(w);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(br#"{"seq":1,"ts_ms":1,"type":"user/message","content":"x"}"#)
        .unwrap();
    let mut w = store.open_writer("s").unwrap();
    assert_eq!(w.next_seq(), 2);
    w.append(user("y")).unwrap();
    assert_eq!(store.read("s").unwrap().len(), 3);
}

#[test]
fn mid_log_corruption_is_error() {
    let (_d, store) = store();
    let w = store
        .create(NewSession {
            id: Some("s".into()),
            ..Default::default()
        })
        .unwrap();
    let path = w.path().to_owned();
    drop(w);
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("garbage\n");
    text.push_str(r#"{"seq":2,"ts_ms":1,"type":"user/message","content":"x"}"#);
    text.push('\n');
    std::fs::write(&path, text).unwrap();
    assert!(matches!(
        store.read("s"),
        Err(SessionError::Corrupt { line: 2, .. })
    ));
}

fn seeded(store: &SessionStore) {
    let mut w = store
        .create(NewSession {
            id: Some("src".into()),
            title: Some("Src".into()),
            model: Some("m".into()),
            ..Default::default()
        })
        .unwrap();
    for (t, q, a) in [("t1", "q1", "a1"), ("t2", "q2", "a2")] {
        w.append(turn_start(t)).unwrap();
        w.append(user(q)).unwrap();
        w.append(asst(a)).unwrap();
        w.append(turn_end(t)).unwrap();
    }
    w.append(turn_start("t3")).unwrap();
    w.append(user("q3")).unwrap();
    w.sync().unwrap();
}

fn texts(events: &[SessionEvent]) -> Vec<String> {
    derive_messages(events)
        .into_iter()
        .map(|m| m.content)
        .collect()
}

#[test]
fn fork_after_turn_copies_prefix() {
    let (_d, store) = store();
    seeded(&store);
    let mut f = store
        .fork(
            "src",
            &ForkPoint::AfterTurn("t1".into()),
            NewSession {
                id: Some("f".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let events = store.read("f").unwrap();
    assert_eq!(texts(&events), ["q1", "a1"]);
    // Parent link + inherited metadata.
    match &events[0].kind {
        EventKind::SessionStart {
            parent,
            title,
            model,
            ..
        } => {
            assert_eq!(
                parent.as_ref().unwrap(),
                &ParentRef {
                    session_id: "src".into(),
                    seq: 4
                }
            );
            assert_eq!(title.as_deref(), Some("Src"));
            assert_eq!(model.as_deref(), Some("m"));
        }
        k => panic!("{k:?}"),
    }
    // Original seqs preserved; fork is appendable and diverges from source.
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        [0, 1, 2, 3, 4]
    );
    f.append(turn_start("t9")).unwrap();
    f.append(user("other")).unwrap();
    f.sync().unwrap();
    assert_eq!(texts(&store.read("f").unwrap()), ["q1", "a1", "other"]);
    assert_eq!(
        texts(&store.read("src").unwrap()),
        ["q1", "a1", "q2", "a2", "q3"]
    );
    let row = store.index().get("f").unwrap().unwrap();
    assert_eq!(row.parent.unwrap().session_id, "src");
}

#[test]
fn fork_before_turn_and_errors() {
    let (_d, store) = store();
    seeded(&store);
    store
        .fork(
            "src",
            &ForkPoint::BeforeTurn("t2".into()),
            NewSession {
                id: Some("f".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(texts(&store.read("f").unwrap()), ["q1", "a1"]);

    let n = || NewSession::default();
    assert!(matches!(
        store.fork("src", &ForkPoint::AfterTurn("t3".into()), n()),
        Err(SessionError::TurnOpen(_))
    ));
    assert!(matches!(
        store.fork("src", &ForkPoint::AfterTurn("nope".into()), n()),
        Err(SessionError::TurnNotFound(_))
    ));
    assert!(matches!(
        store.fork("nope", &ForkPoint::AfterTurn("t1".into()), n()),
        Err(SessionError::NotFound(_))
    ));
}

#[test]
fn rebuild_index_matches_live_rows_and_ids_are_checked() {
    let (_d, store) = store();
    seeded(&store);
    let live = store.index().get("src").unwrap().unwrap();
    store.index().remove("src").unwrap();
    assert_eq!(store.rebuild_index().unwrap(), 1);
    assert_eq!(store.index().get("src").unwrap().unwrap(), live);
    assert_eq!(store.index().list(10, 0).unwrap().len(), 1);

    assert!(matches!(
        store.read("../etc"),
        Err(SessionError::InvalidId(_))
    ));
    assert!(matches!(
        store.create(NewSession {
            id: Some("src".into()),
            ..Default::default()
        }),
        Err(SessionError::AlreadyExists(_))
    ));
}
