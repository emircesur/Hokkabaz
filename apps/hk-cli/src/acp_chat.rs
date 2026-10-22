//! `hk chat --engine <name|cmd>`: line-mode chat where an external ACP agent (dsh, opencode,
//! any `--engine "cmd args"`) does the work. Turns are logged to the normal session store.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use hk_acp::{
    AcpEngine, ApprovalPrompt, Approver, Decision, EngineEvent, EngineOptions, EngineSpec, SessionMapper,
    StopReason, ToolInfo,
};
use hk_core::{Config, LoadOptions, Profile};
use hk_session::{NewSession, SessionStore};
use tokio::sync::{mpsc, oneshot};

use crate::chat::ChatArgs;
use crate::paths::Paths;
use crate::term::{clip, dim, err, out};

type ApprovalAsk = (ApprovalPrompt, oneshot::Sender<Decision>);

/// Routes agent permission requests to the REPL loop, which owns stdin.
struct CliApprover(mpsc::UnboundedSender<ApprovalAsk>);

#[async_trait]
impl Approver for CliApprover {
    async fn decide(&self, p: &ApprovalPrompt) -> Decision {
        let (tx, rx) = oneshot::channel();
        if self.0.send((p.clone(), tx)).is_err() {
            return Decision::Deny;
        }
        rx.await.unwrap_or(Decision::Deny)
    }
}

pub async fn run(paths: &Paths, args: ChatArgs, engine_arg: &str) -> Result<()> {
    if args.session.is_some() || args.resume {
        bail!("--session/--resume are not supported with --engine (external agent owns its session)");
    }
    let cwd = std::env::current_dir().context("cwd")?;
    let mut opts = LoadOptions::new(Profile::Cli).with_process_env();
    opts.user_path = Some(paths.data.join("hokkabaz.toml"));
    opts.project_path = Some(cwd.join(".hokkabaz").join("config.toml"));
    let cfg = Config::load(&opts).context("load config")?;
    let spec = EngineSpec::resolve(Some(&cfg), engine_arg)?;

    let (appr_tx, mut appr_rx) = mpsc::unbounded_channel::<ApprovalAsk>();
    let mut engine = AcpEngine::spawn(&spec, EngineOptions::new(cwd.clone(), Arc::new(CliApprover(appr_tx))))
        .await
        .with_context(|| format!("start engine `{}`", spec.name))?;
    let mut events = engine.take_events().context("engine events")?;

    let store = SessionStore::open(paths.sessions()).context("open session store")?;
    let mut writer = store.create(NewSession {
        cwd: Some(cwd.display().to_string()),
        model: Some(format!("acp:{}", spec.name)),
        ..NewSession::default()
    })?;
    let agent = engine
        .agent_info()
        .map(|i| format!("{} {}", i.name, i.version))
        .unwrap_or_else(|| spec.name.clone());
    err(&format!("session {}  engine {} ({agent})\n", writer.session_id(), spec.name));

    let mut lines = stdin_lines();
    let oneshot = !args.prompt.is_empty();
    let mut next = oneshot.then(|| args.prompt.join(" "));
    let mut ui = Ui {
        show_reasoning: args.show_reasoning,
        mid_line: false,
    };
    let mut failed = false;
    let mut turn_no = 0u32;
    loop {
        let input = match next.take() {
            Some(p) => p,
            None if oneshot => break,
            None => match read_prompt(&mut lines).await {
                Some(l) => l,
                None => break,
            },
        };
        let input = input.trim();
        if input.is_empty() {
            continue;
        }
        if matches!(input, "/exit" | "/quit") {
            break;
        }

        let mut mapper = SessionMapper::new(format!("turn_{turn_no}"));
        turn_no += 1;
        log(&mut writer, mapper.begin(input));

        let stop = {
            let fut = engine.prompt(input);
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    r = &mut fut => break r,
                    Some(ev) = events.recv() => {
                        ui.render(&ev);
                        log(&mut writer, mapper.push(&ev));
                    }
                    Some((p, tx)) = appr_rx.recv() => {
                        let _ = tx.send(ask(&mut lines, &mut ui, &p).await);
                    }
                    _ = tokio::signal::ctrl_c() => engine.cancel(),
                }
            }
        };
        // Updates precede the prompt response on the wire: drain what is already queued.
        while let Ok(ev) = events.try_recv() {
            ui.render(&ev);
            log(&mut writer, mapper.push(&ev));
        }
        ui.newline();
        match stop {
            Ok(s) => {
                log(&mut writer, mapper.finish(s));
                failed |= s == StopReason::Refusal;
                err(&dim(&format!("[{s:?}]\n")));
            }
            Err(e) => {
                log(&mut writer, mapper.fail());
                err(&format!("error: {e}\n"));
                let tail = engine.stderr_tail();
                if !tail.is_empty() {
                    err(&dim(&format!("agent stderr: {}\n", tail.join(" | "))));
                }
                failed = true;
                break;
            }
        }
        if let Err(e) = writer.sync() {
            err(&format!("session log: {e}\n"));
        }
    }
    engine.shutdown().await;
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn log(w: &mut hk_session::SessionWriter, kinds: Vec<hk_session::EventKind>) {
    for k in kinds {
        if let Err(e) = w.append(k) {
            err(&format!("session log: {e}\n"));
            return;
        }
    }
}

struct Ui {
    show_reasoning: bool,
    mid_line: bool,
}

impl Ui {
    fn text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        out(s);
        self.mid_line = !s.ends_with('\n');
    }

    fn newline(&mut self) {
        if std::mem::take(&mut self.mid_line) {
            out("\n");
        }
    }

    fn line(&mut self, s: &str) {
        self.newline();
        out(&format!("{s}\n"));
    }

    fn render(&mut self, ev: &EngineEvent) {
        match ev {
            EngineEvent::Text(t) => self.text(t),
            EngineEvent::Thought(t) if self.show_reasoning => self.text(&dim(t)),
            EngineEvent::Thought(_) | EngineEvent::ApprovalRequested(_) | EngineEvent::ApprovalResolved { .. } => {}
            EngineEvent::ToolCall(t) => self.line(&dim(&format!("> {}", tool_label(t)))),
            EngineEvent::ToolUpdate(t) => {
                if let Some(s) = t.status.filter(|s| s.is_terminal()) {
                    let tag = if s == hk_acp::proto::ToolStatus::Failed { "error" } else { "ok" };
                    let body = t.content.as_deref().unwrap_or_default();
                    self.line(&dim(&format!("  = {tag}: {}", clip(&body.replace('\n', " | "), 400))));
                }
            }
            EngineEvent::Plan(p) => {
                for e in p {
                    self.line(&dim(&format!("  [{}] {}", e.status.as_deref().unwrap_or("?"), e.content)));
                }
            }
        }
    }
}

fn tool_label(t: &ToolInfo) -> String {
    let name = t.title.as_deref().or(t.kind.as_deref()).unwrap_or("tool");
    match &t.raw_input {
        Some(v) => format!("{name} {}", clip(&v.to_string(), 200)),
        None => name.to_owned(),
    }
}

async fn ask(lines: &mut mpsc::UnboundedReceiver<String>, ui: &mut Ui, p: &ApprovalPrompt) -> Decision {
    ui.newline();
    err(&format!(
        "approve {} {}? [y/N/a=always] ",
        p.title,
        p.detail.as_deref().unwrap_or("")
    ));
    let answer = tokio::select! {
        l = lines.recv() => l,
        _ = tokio::signal::ctrl_c() => None,
    };
    // EOF / interrupt count as a denial: never auto-approve.
    match answer.as_deref().map(|a| a.trim().to_ascii_lowercase()).as_deref() {
        Some("y" | "yes") => Decision::AllowOnce,
        Some("a" | "always") => Decision::AllowAlways,
        _ => Decision::Deny,
    }
}

async fn read_prompt(lines: &mut mpsc::UnboundedReceiver<String>) -> Option<String> {
    out("> ");
    tokio::select! {
        l = lines.recv() => l,
        _ = tokio::signal::ctrl_c() => {
            out("\n");
            None
        }
    }
}

/// Blocking stdin on its own thread so one reader serves both the REPL and approvals.
fn stdin_lines() -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}
