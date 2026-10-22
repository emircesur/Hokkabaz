//! `hk tui`: full-screen terminal UI over `hk_server::LocalRuntime`.
//!
//! `state` is the pure reducer, `view` the pure renderer; this file is the only impure part:
//! terminal setup, the crossterm reader thread, the runtime feed and command execution.

mod state;
mod view;

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use hk_server::{
    AgentRuntime, ApprovalDecision, CreateSession, LocalConfig, LocalRuntime, TurnMode,
};
use ratatui::crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use ratatui::crossterm::execute;
use tokio::sync::{broadcast, mpsc};

use self::state::{Cmd, Conn, Msg, TuiState};
use crate::auth;
use crate::chat::{DEFAULT_MODEL, ModeArg};
use crate::paths::Paths;

#[derive(Debug, clap::Args)]
pub struct TuiArgs {
    /// Start in this mode: `build` (all tools) or `plan` (read-only fs).
    #[arg(long, value_enum, default_value = "build")]
    pub mode: ModeArg,
    /// Provider-local model id for new sessions.
    #[arg(long, default_value = DEFAULT_MODEL)]
    pub model: String,
    /// Override the API endpoint (gateway, mock server).
    #[arg(long, env = "DEEPSEEK_BASE_URL")]
    pub base_url: Option<String>,
    /// Open an existing session (id or unique prefix).
    #[arg(long, conflicts_with = "resume")]
    pub session: Option<String>,
    /// Open the most recently updated session.
    #[arg(long)]
    pub resume: bool,
    /// Max model requests per turn.
    #[arg(long, default_value_t = 50)]
    pub max_steps: u32,
}

impl Default for TuiArgs {
    fn default() -> Self {
        Self {
            mode: ModeArg::Build,
            model: DEFAULT_MODEL.to_owned(),
            base_url: std::env::var("DEEPSEEK_BASE_URL")
                .ok()
                .filter(|u| !u.is_empty()),
            session: None,
            resume: false,
            max_steps: 50,
        }
    }
}

pub async fn run(paths: &Paths, args: TuiArgs) -> Result<()> {
    let key = auth::deepseek_key()?;
    let mut provider = hk_llm::deepseek::DeepSeek::new(key).context("build DeepSeek client")?;
    if let Some(u) = &args.base_url {
        provider = provider.base_url(u);
    }
    std::fs::create_dir_all(paths.spill()).context("create spill dir")?;
    let rt = LocalRuntime::new(LocalConfig {
        provider: Arc::new(provider),
        sessions_dir: paths.sessions(),
        spill_dir: paths.spill(),
        default_model: args.model.clone(),
        default_cwd: std::env::current_dir().ok(),
        max_steps: args.max_steps,
    })
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    let list = rt
        .list_sessions(500, 0)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let open = match (&args.session, args.resume) {
        (Some(prefix), _) => {
            let hits: Vec<_> = list
                .iter()
                .filter(|s| s.id.starts_with(prefix.as_str()))
                .collect();
            match hits.as_slice() {
                [one] => Some(one.id.clone()),
                [] => bail!("no session matches `{prefix}`"),
                _ => bail!("`{prefix}` is ambiguous ({} sessions match)", hits.len()),
            }
        }
        (None, true) => Some(
            list.first()
                .map(|s| s.id.clone())
                .context("no sessions to resume")?,
        ),
        _ => None,
    };

    let mode = match args.mode {
        ModeArg::Build => TurnMode::Build,
        ModeArg::Plan => TurnMode::Plan,
    };
    let mut st = TuiState::new(args.model.clone(), mode);
    let mut first_cmds = Vec::new();
    st.update(Msg::Sessions(list));
    if let Some(id) = open {
        if let Some(m) = st
            .sessions
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.model.clone())
        {
            st.model = m;
        }
        st.begin_session(&id, true);
        first_cmds.push(Cmd::Load(id));
    }

    // Subscribe before anything runs so no live event is missed.
    let feed = rt.subscribe();
    let (res_tx, res_rx) = mpsc::unbounded_channel::<Msg>();
    let (key_tx, key_rx) = mpsc::unbounded_channel::<Event>();
    // Blocking reads on their own thread; process exit reaps it.
    std::thread::spawn(move || {
        while let Ok(ev) = ratatui::crossterm::event::read() {
            if key_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let mut terminal = ratatui::init();
    // ratatui::init installed a panic hook that restores the terminal; also drop paste mode.
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableBracketedPaste);
        prev(info);
    }));

    let res = event_loop(
        &mut terminal,
        &rt,
        st,
        first_cmds,
        feed,
        key_rx,
        res_tx,
        res_rx,
    )
    .await;

    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    res
}

#[allow(clippy::too_many_arguments)]
async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    rt: &LocalRuntime,
    mut st: TuiState,
    first_cmds: Vec<Cmd>,
    mut feed: broadcast::Receiver<hk_server::ServerEvent>,
    mut keys: mpsc::UnboundedReceiver<Event>,
    res_tx: mpsc::UnboundedSender<Msg>,
    mut res_rx: mpsc::UnboundedReceiver<Msg>,
) -> Result<()> {
    if let Ok(size) = terminal.size() {
        st.update(Msg::Resize {
            height: size.height,
        });
    }
    // Models for the picker; sessions list already loaded.
    spawn_models(rt, &res_tx);
    for c in first_cmds {
        if exec(rt, c, &res_tx).await {
            return Ok(());
        }
    }

    loop {
        terminal.draw(|f| view::draw(f, &st)).context("draw")?;
        let mut msgs = Vec::new();
        tokio::select! {
            ev = keys.recv() => match ev {
                Some(Event::Key(k)) if k.kind != KeyEventKind::Release => msgs.push(Msg::Key(k)),
                Some(Event::Paste(p)) => msgs.push(Msg::Paste(p)),
                Some(Event::Resize(_, h)) => msgs.push(Msg::Resize { height: h }),
                Some(_) => {}
                None => return Ok(()),
            },
            m = res_rx.recv() => msgs.extend(m),
            r = feed.recv() => feed_msgs(r, &st, &mut msgs),
        }
        // Burst of deltas: fold them in before the next draw.
        for _ in 0..512 {
            match feed.try_recv() {
                Ok(ev) => feed_msgs(Ok(ev), &st, &mut msgs),
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    feed_msgs(Err(broadcast::error::RecvError::Lagged(n)), &st, &mut msgs)
                }
                Err(broadcast::error::TryRecvError::Closed) => break,
            }
        }
        for m in msgs {
            for c in st.update(m) {
                if exec(rt, c, &res_tx).await {
                    return Ok(());
                }
            }
        }
    }
}

fn feed_msgs(
    r: Result<hk_server::ServerEvent, broadcast::error::RecvError>,
    st: &TuiState,
    out: &mut Vec<Msg>,
) {
    match r {
        Ok(ev) => {
            if st.current.as_deref() == Some(ev.session_id.as_str()) {
                out.extend(Msg::from_feed(&ev));
            }
        }
        Err(broadcast::error::RecvError::Lagged(_)) => out.push(Msg::Conn(Conn::Lagged)),
        Err(broadcast::error::RecvError::Closed) => out.push(Msg::Conn(Conn::Closed)),
    }
}

fn spawn_models(rt: &LocalRuntime, tx: &mpsc::UnboundedSender<Msg>) {
    let (rt, tx) = (rt.clone(), tx.clone());
    tokio::spawn(async move {
        match rt.models().await {
            Ok(m) => drop(tx.send(Msg::Models(m))),
            Err(e) => drop(tx.send(Msg::Error(format!("models: {e}")))),
        }
    });
}

/// Run one command; `true` means quit.
async fn exec(rt: &LocalRuntime, cmd: Cmd, tx: &mpsc::UnboundedSender<Msg>) -> bool {
    let fail =
        |what: &str, e: hk_server::ApiError| drop(tx.send(Msg::Error(format!("{what}: {e}"))));
    match cmd {
        Cmd::Quit => return true,
        // Inline: the runtime call is a flag flip, and quit must not outrun it.
        Cmd::Cancel(id) => {
            if let Err(e) = rt.cancel(&id).await {
                fail("cancel", e);
            }
        }
        Cmd::Respond {
            approval_id,
            session_id,
            approved,
            reason,
        } => {
            let d = ApprovalDecision {
                approved,
                reason,
                session_id: Some(session_id),
            };
            if let Err(e) = rt.respond_approval(&approval_id, d).await {
                fail("approval", e);
            }
        }
        Cmd::Submit { session_id, req } => {
            if let Err(e) = rt.submit(&session_id, req).await {
                fail("submit", e);
            }
        }
        Cmd::CreateSession { model } => {
            let (rt, tx) = (rt.clone(), tx.clone());
            tokio::spawn(async move {
                let req = CreateSession {
                    cwd: std::env::current_dir()
                        .ok()
                        .map(|p| p.display().to_string()),
                    model: Some(model).filter(|m| !m.is_empty()),
                    title: None,
                };
                let _ = match rt.create_session(req).await {
                    Ok(info) => tx.send(Msg::Created(Box::new(info))),
                    Err(e) => tx.send(Msg::Error(format!("new session: {e}"))),
                };
            });
        }
        Cmd::Load(id) => {
            let (rt, tx) = (rt.clone(), tx.clone());
            tokio::spawn(async move {
                let _ = match rt.session(&id, None).await {
                    Ok(d) => tx.send(Msg::Loaded {
                        session_id: id,
                        events: d.events,
                        pending: d.pending_approvals,
                    }),
                    Err(e) => tx.send(Msg::Error(format!("load session: {e}"))),
                };
            });
        }
        Cmd::RefreshSessions => {
            let (rt, tx) = (rt.clone(), tx.clone());
            tokio::spawn(async move {
                if let Ok(l) = rt.list_sessions(500, 0).await {
                    let _ = tx.send(Msg::Sessions(l));
                }
            });
        }
    }
    false
}
