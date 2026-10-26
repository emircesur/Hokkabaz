//! `RemoteCore` against the real hk-server axum surface on loopback: pairing -> connect ->
//! create_session -> submit_turn -> live events, then a dropped socket resuming from the log.
//! The model is scripted; nothing leaves 127.0.0.1.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use hk_ffi::dto::{
    CoreProfile, CreateSessionDto, InstanceStateDto, RoleDto, SessionEventDto,
    SessionEventKind as K, TurnEndReasonDto, TurnRequestDto,
};
use hk_ffi::error::HkErrorKind;
use hk_ffi::handle::{self, CoreHandle};
use hk_ffi::instances::{LOCAL_ID, Manager};
use hk_ffi::local::{CoreOptions, LocalCore};
use hk_ffi::remote::RemoteCore;
use hk_llm::{
    CancellationToken, Capabilities, ChatRequest, EventStream, FinishReason, ModelInfo, Provider,
    StreamEvent,
};
use hk_server::{
    AppState, Auth, LocalConfig, LocalRuntime, Pairing, Scope, TurnRequest, serve_listener,
};
use hk_store::{MemorySecretStore, Secret};
use tokio::net::TcpListener;

type Script = Vec<hk_llm::Result<StreamEvent>>;

struct Scripted(Mutex<Vec<Script>>);

#[async_trait::async_trait]
impl Provider for Scripted {
    fn id(&self) -> &str {
        "fake"
    }

    async fn models(&self) -> hk_llm::Result<Vec<ModelInfo>> {
        Ok(vec![ModelInfo {
            id: "m".into(),
            provider: "fake".into(),
            name: "Fake".into(),
            context_window: 8192,
            max_output: None,
            capabilities: Capabilities {
                tools: true,
                reasoning: false,
                vision: false,
            },
            cost: None,
        }])
    }

    async fn stream(
        &self,
        _req: ChatRequest,
        _cancel: CancellationToken,
    ) -> hk_llm::Result<EventStream> {
        let script = {
            let mut v = self.0.lock().unwrap();
            if v.is_empty() {
                Vec::new()
            } else {
                v.remove(0)
            }
        };
        Ok(futures::stream::iter(script).boxed())
    }
}

fn say(parts: &[&str]) -> Script {
    let mut s: Script = parts
        .iter()
        .map(|p| Ok(StreamEvent::TextDelta { text: (*p).into() }))
        .collect();
    s.push(Ok(StreamEvent::Finish(FinishReason::Stop)));
    s
}

const RUN: &str = "run-token-0123456789";
const ADMIN: &str = "admin-token-0123456789";

struct Daemon {
    url: String,
    auth: Auth,
    runtime: Arc<LocalRuntime>,
    _dir: tempfile::TempDir,
}

/// hokkabazd-like server: real runtime + auth + pairing behind `serve_listener`.
async fn daemon(scripts: Vec<Script>) -> Daemon {
    let dir = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let runtime = Arc::new(
        LocalRuntime::new(LocalConfig {
            provider: Arc::new(Scripted(Mutex::new(scripts))),
            sessions_dir: dir.path().join("sessions"),
            spill_dir: dir.path().join("spill"),
            default_model: "m".into(),
            default_cwd: Some(dir.path().to_path_buf()),
            max_steps: 4,
        })
        .unwrap(),
    );
    let auth = Auth::new()
        .with_token(Scope::Run, RUN)
        .with_token(Scope::Admin, ADMIN);
    let state = AppState::new(runtime.clone(), auth.clone())
        .with_pairing(Pairing::new(&auth, &url).unwrap());
    tokio::spawn(serve_listener(listener, state, std::future::pending()));
    Daemon {
        url,
        auth,
        runtime,
        _dir: dir,
    }
}

async fn until(feed: &mut BoxStream<'static, SessionEventDto>, want: K) -> Vec<SessionEventDto> {
    let mut seen = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), feed.next())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out waiting for {want:?}; saw {:?}",
                    seen.iter()
                        .map(|e: &SessionEventDto| e.kind)
                        .collect::<Vec<_>>()
                )
            })
            .expect("feed ended");
        let done = ev.kind == want;
        seen.push(ev);
        if done {
            return seen;
        }
    }
}

#[tokio::test]
async fn pair_connect_turn_and_swap_back() {
    let d = daemon(vec![say(&["he", "llo"])]).await;
    let data = tempfile::tempdir().unwrap();
    let local = LocalCore::open(
        data.path().to_path_buf(),
        CoreProfile::Desktop,
        CoreOptions {
            secrets: Some(Arc::new(MemorySecretStore::default())),
            extra_providers: Vec::new(),
            refresh_catalog: false,
        },
    )
    .await
    .unwrap();
    handle::install_local(local);
    let secrets = Arc::new(MemorySecretStore::default());
    let mgr = Manager::open(data.path(), CoreProfile::Desktop, secrets);

    // Pair from the link the daemon would show as QR; it is single use.
    let link = Pairing::new(&d.auth, &d.url)
        .unwrap()
        .mint(Scope::Run)
        .unwrap()
        .payload;
    let inst = mgr.pair(&link, Some("box")).await.unwrap();
    assert_eq!(
        (inst.name.as_str(), inst.has_token, inst.connected),
        ("box", true, false)
    );
    assert_eq!(
        mgr.pair(&link, None).await.unwrap_err().kind,
        HkErrorKind::Unauthorized,
        "replay"
    );
    assert_eq!(
        mgr.status(&inst.id).await.unwrap().state,
        InstanceStateDto::Online
    );

    let on = mgr.connect(&inst.id).await.unwrap();
    assert!(on.connected && on.is_default);
    let core = handle::core().unwrap();

    // Provider setup belongs to the daemon.
    assert_eq!(
        core.get_settings().await.unwrap_err().kind,
        HkErrorKind::Unsupported
    );
    assert_eq!(
        core.list_models().await.unwrap().models[0].reference,
        "fake/m"
    );

    let s = core
        .create_session(CreateSessionDto {
            title: Some("t".into()),
            ..CreateSessionDto::default()
        })
        .await
        .unwrap();
    assert_eq!(core.list_sessions(10, 0).await.unwrap().len(), 1);
    assert_eq!(
        core.get_session("nope").await.unwrap_err().kind,
        HkErrorKind::NotFound
    );

    let mut feed = core.session_events(&s.id).unwrap();
    let accepted = core
        .submit_turn(
            &s.id,
            TurnRequestDto {
                prompt: "hi".into(),
                mode: None,
                model: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (accepted.session_id.as_str(), accepted.queued),
        (s.id.as_str(), false)
    );
    let seen = until(&mut feed, K::TurnEnded).await;
    assert!(
        seen.iter()
            .any(|e| e.kind == K::UserMessage && e.text.as_deref() == Some("hi"))
    );
    assert!(
        seen.iter().any(|e| e.kind == K::TextDelta),
        "streamed deltas arrive live"
    );
    let msg = seen
        .iter()
        .find(|e| e.kind == K::AssistantMessage)
        .expect("assistant message");
    assert_eq!(msg.text.as_deref(), Some("hello"));
    assert_eq!(
        seen.last().unwrap().end_reason,
        Some(TurnEndReasonDto::Completed)
    );

    let detail = core.get_session(&s.id).await.unwrap();
    assert_eq!(
        detail.messages.last().map(|m| (m.role, m.content.as_str())),
        Some((RoleDto::Assistant, "hello"))
    );
    assert!(detail.pending_approvals.is_empty());

    // Swapping out shuts the remote down (its feed ends); the local core is back, untouched.
    mgr.connect(LOCAL_ID).await.unwrap();
    assert!(handle::local_active());
    assert!(
        tokio::time::timeout(Duration::from_secs(5), feed.next())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        handle::core()
            .unwrap()
            .list_sessions(10, 0)
            .await
            .unwrap()
            .len(),
        0
    );

    // A bad token never becomes the active core.
    let bad = mgr
        .add("bad", &d.url.replace("127.0.0.1", "localhost"), "nope")
        .unwrap();
    assert_eq!(
        mgr.connect(&bad.id).await.unwrap_err().kind,
        HkErrorKind::Unauthorized
    );
    assert!(handle::local_active());
    mgr.remove(&bad.id).await.unwrap();
    assert_eq!(mgr.list().len(), 2);
}

/// TCP relay whose connections can be cut and the listener reopened on the same port, to model
/// a flaky link without touching the daemon.
struct Flaky {
    port: u16,
    cut: tokio::sync::watch::Sender<u64>,
}

impl Flaky {
    async fn start(upstream: &str) -> Self {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (cut, _) = tokio::sync::watch::channel(0u64);
        let f = Self { port, cut };
        f.serve(l, upstream.trim_start_matches("http://").to_owned());
        f
    }

    fn serve(&self, l: TcpListener, upstream: String) {
        let mut gen_rx = self.cut.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = gen_rx.changed() => return,
                    r = l.accept() => {
                        let Ok((mut c, _)) = r else { return };
                        let up = upstream.clone();
                        let mut g = gen_rx.clone();
                        tokio::spawn(async move {
                            let Ok(mut s) = tokio::net::TcpStream::connect(up).await else { return };
                            tokio::select! {
                                _ = g.changed() => {}
                                _ = tokio::io::copy_bidirectional(&mut c, &mut s) => {}
                            }
                        });
                    }
                }
            }
        });
    }

    /// Drop every open connection and stop listening.
    fn cut(&self) {
        self.cut.send_modify(|g| *g += 1);
    }

    async fn reopen(&self, upstream: &str) {
        let l = loop {
            match TcpListener::bind(("127.0.0.1", self.port)).await {
                Ok(l) => break l,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        };
        self.serve(l, upstream.trim_start_matches("http://").to_owned());
    }
}

#[tokio::test]
async fn feed_resumes_after_link_drop() {
    let d = daemon(vec![say(&["one"]), say(&["two"])]).await;
    let relay = Flaky::start(&d.url).await;
    let core = RemoteCore::new(
        &format!("http://127.0.0.1:{}", relay.port),
        Secret::new(RUN),
        CoreProfile::Desktop,
    )
    .unwrap();

    let s = core
        .create_session(CreateSessionDto::default())
        .await
        .unwrap();
    let mut feed = core.session_events(&s.id).unwrap();
    // Give the first socket time to subscribe, then run turn one live.
    tokio::time::sleep(Duration::from_millis(300)).await;
    core.submit_turn(
        &s.id,
        TurnRequestDto {
            prompt: "a".into(),
            mode: None,
            model: None,
        },
    )
    .await
    .unwrap();
    until(&mut feed, K::TurnEnded).await;

    // Link dies; turn two happens while the app cannot hear it.
    relay.cut();
    tokio::time::sleep(Duration::from_millis(200)).await;
    use hk_server::AgentRuntime;
    d.runtime
        .submit(
            &s.id,
            TurnRequest { images: Vec::new(),
                prompt: "b".into(),
                mode: None,
                model: None,
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while d
            .runtime
            .list_sessions(10, 0)
            .await
            .unwrap()
            .iter()
            .any(|x| x.running)
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();

    relay.reopen(&d.url).await;
    let seen = until(&mut feed, K::TurnEnded).await;
    assert!(
        seen.iter().any(|e| e.kind == K::Error && !e.fatal),
        "outage is announced once"
    );
    assert!(
        seen.iter()
            .any(|e| e.kind == K::UserMessage && e.text.as_deref() == Some("b")),
        "missed events replayed"
    );
    let finals: Vec<_> = seen
        .iter()
        .filter(|e| e.kind == K::AssistantMessage)
        .collect();
    assert_eq!(finals.len(), 1);
    assert_eq!(finals[0].text.as_deref(), Some("two"));
    // No turn-one event comes back after the resume.
    assert!(
        !seen
            .iter()
            .any(|e| e.text.as_deref() == Some("one") || e.text.as_deref() == Some("a"))
    );

    core.shutdown().await;
    assert!(
        tokio::time::timeout(Duration::from_secs(5), feed.next())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn bad_token_ends_feed_with_fatal_error() {
    let d = daemon(Vec::new()).await;
    let core = RemoteCore::new(&d.url, Secret::new("wrong"), CoreProfile::Desktop).unwrap();
    assert_eq!(
        core.list_sessions(1, 0).await.unwrap_err().kind,
        HkErrorKind::Unauthorized
    );
    let mut feed = core.session_events("any").unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), feed.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((ev.kind, ev.fatal), (K::Error, true));
    assert!(feed.next().await.is_none());
}
