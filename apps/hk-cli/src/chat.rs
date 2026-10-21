//! `hk chat`: line-mode REPL over `hk_agent::Agent`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context, Result};
use hk_agent::{Agent, AgentConfig, AgentHandle, AgentStream, ApprovalRequest, Mode, TurnSummary};
use hk_core::{Effect, EventBus};
use hk_llm::StreamEvent;
use hk_llm::deepseek::DeepSeek;
use hk_session::{EventKind, NewSession, SessionStore, TurnEndReason};
use hk_tools::builtin::register_builtins;
use hk_tools::{OutputLimits, ToolExecute, ToolPipeline, ToolPostExecute, ToolRegistry};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::paths::Paths;
use crate::term::{clip, dim, err, out};
use crate::{auth, sessions};

const SYSTEM_PROMPT: &str = "You are Hokkabaz, a coding agent running in the user's terminal. \
Use the available tools to inspect and change files in the working directory. Be concise.";

pub const DEFAULT_MODEL: &str = "deepseek-chat";

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ModeArg {
    Build,
    Plan,
}

#[derive(Debug, clap::Args)]
pub struct ChatArgs {
    /// One-shot prompt; omit for an interactive REPL.
    pub prompt: Vec<String>,
    /// Tool policy: `build` (all tools) or `plan` (read-only fs).
    #[arg(long, value_enum, default_value = "build")]
    pub mode: ModeArg,
    /// Provider-local model id.
    #[arg(long, default_value = DEFAULT_MODEL)]
    pub model: String,
    /// Override the API endpoint (gateway, mock server).
    #[arg(long, env = "DEEPSEEK_BASE_URL")]
    pub base_url: Option<String>,
    /// Continue an existing session (id or unique prefix).
    #[arg(long, conflicts_with = "resume")]
    pub session: Option<String>,
    /// Continue the most recently updated session.
    #[arg(long)]
    pub resume: bool,
    /// Max model requests per turn.
    #[arg(long, default_value_t = 50)]
    pub max_steps: u32,
    /// Print model reasoning (dimmed) while it streams.
    #[arg(long)]
    pub show_reasoning: bool,
}

/// Live-output state shared by the bus listeners.
#[derive(Default)]
struct Ui {
    /// Text was written without a trailing newline.
    mid_line: AtomicBool,
    /// Call ids whose result was already printed.
    shown: Mutex<HashSet<String>>,
    show_reasoning: bool,
}

impl Ui {
    fn text(&self, s: &str) {
        if s.is_empty() {
            return;
        }
        out(s);
        self.mid_line.store(!s.ends_with('\n'), Ordering::Relaxed);
    }

    fn newline(&self) {
        if self.mid_line.swap(false, Ordering::Relaxed) {
            out("\n");
        }
    }

    fn line(&self, s: &str) {
        self.newline();
        out(&format!("{s}\n"));
    }

    fn shown(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.shown.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub async fn run(paths: &Paths, args: ChatArgs) -> Result<()> {
    let key = auth::deepseek_key()?;
    let mut provider = DeepSeek::new(key).context("build DeepSeek client")?;
    if let Some(u) = &args.base_url {
        provider = provider.base_url(u);
    }

    let store = SessionStore::open(paths.sessions()).context("open session store")?;
    let cwd = std::env::current_dir().context("cwd")?;
    let writer = if let Some(prefix) = &args.session {
        store.open_writer(&sessions::resolve(&store, prefix)?)?
    } else if args.resume {
        store.open_writer(&sessions::latest(&store)?)?
    } else {
        store.create(NewSession {
            cwd: Some(cwd.display().to_string()),
            model: Some(args.model.clone()),
            ..NewSession::default()
        })?
    };
    let session_id = writer.session_id().to_owned();

    let bus = EventBus::new();
    let registry = ToolRegistry::new();
    let mut effects: Vec<Effect> = register_builtins(&registry)?;
    let pipeline = ToolPipeline::new(registry, bus.clone(), OutputLimits::new(paths.spill()));

    let mut cfg = AgentConfig::new(args.model.clone());
    cfg.system = vec![SYSTEM_PROMPT.to_owned()];
    cfg.cwd = Some(cwd);
    cfg.max_steps = args.max_steps;
    cfg.mode = match args.mode {
        ModeArg::Build => Mode::Build,
        ModeArg::Plan => Mode::Plan,
    };
    let mut agent = Agent::new(Arc::new(provider), pipeline, bus.clone(), cfg, writer)?;
    let handle = agent.handle();

    let ui = Arc::new(Ui {
        show_reasoning: args.show_reasoning,
        ..Ui::default()
    });
    let (appr_tx, mut appr_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
    effects.extend(listeners(&bus, &ui, appr_tx));

    err(&format!(
        "session {session_id}  model {}  mode {}\n",
        args.model,
        match args.mode { ModeArg::Build => "build", ModeArg::Plan => "plan" }
    ));

    let mut lines = stdin_lines();
    let oneshot = !args.prompt.is_empty();
    let mut failed = false;
    let mut next = oneshot.then(|| args.prompt.join(" "));
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
        handle.submit(input);
        let before = agent.events().len();
        let res = drive_turn(&mut agent, &handle, &mut appr_rx, &mut lines, &ui).await;
        ui.newline();
        match res {
            Ok(Some(s)) => {
                report_unshown(&agent.events()[before..], &ui);
                failed |= s.reason == TurnEndReason::Failed;
                report_turn(&s);
            }
            Ok(None) => {}
            Err(e) => {
                err(&format!("error: {e}\n"));
                failed = true;
                break;
            }
        }
    }
    drop(effects);
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn listeners(bus: &EventBus, ui: &Arc<Ui>, appr_tx: UnboundedSender<ApprovalRequest>) -> Vec<Effect> {
    let mut v = Vec::new();

    let u = ui.clone();
    v.push(bus.on::<AgentStream, _, _>(move |e| {
        let u = u.clone();
        async move {
            match &e.event {
                StreamEvent::TextDelta { text } => u.text(text),
                StreamEvent::ReasoningDelta { text } if u.show_reasoning => {
                    u.text(&dim(text));
                }
                StreamEvent::Finish(_) => u.newline(),
                _ => {}
            }
        }
    }));

    let u = ui.clone();
    v.push(bus.on::<ToolExecute, _, _>(move |e| {
        let u = u.clone();
        async move {
            u.line(&dim(&format!("> {} {}", e.tool, clip(&e.args.to_string(), 200))));
        }
    }));

    let u = ui.clone();
    v.push(bus.on::<ToolPostExecute, _, _>(move |e| {
        let u = u.clone();
        async move {
            u.shown().insert(e.call_id.clone());
            let tag = if e.output.is_error { "error" } else { "ok" };
            u.line(&dim(&format!("  = {tag}: {}", preview(&e.output.content))));
        }
    }));

    v.push(bus.on::<ApprovalRequest, _, _>(move |e| {
        let tx = appr_tx.clone();
        async move {
            let _ = tx.send((*e).clone());
        }
    }));
    v
}

/// First lines of a tool result, bounded; the full text is in the session log.
fn preview(s: &str) -> String {
    let lines: Vec<&str> = s.lines().take(6).collect();
    let mut t = clip(&lines.join("\n  | "), 600);
    if s.lines().count() > 6 {
        t.push_str("\n  | ...");
    }
    t
}

/// Run one turn while serving approval prompts and Ctrl-C. Ask-risk calls stay parked inside
/// the agent until `respond` is called from here.
async fn drive_turn(
    agent: &mut Agent,
    handle: &AgentHandle,
    appr_rx: &mut UnboundedReceiver<ApprovalRequest>,
    lines: &mut UnboundedReceiver<String>,
    ui: &Ui,
) -> Result<Option<TurnSummary>, hk_agent::AgentError> {
    let fut = agent.run_turn();
    tokio::pin!(fut);
    loop {
        tokio::select! {
            r = &mut fut => return r,
            Some(req) = appr_rx.recv() => ask(handle, lines, ui, &req).await,
            _ = tokio::signal::ctrl_c() => handle.cancel(),
        }
    }
}

async fn ask(handle: &AgentHandle, lines: &mut UnboundedReceiver<String>, ui: &Ui, req: &ApprovalRequest) {
    ui.newline();
    err(&format!(
        "approve {} ({:?}) {}? [y/N] ",
        req.tool,
        req.risk,
        req.detail.as_deref().unwrap_or("")
    ));
    let answer = tokio::select! {
        l = lines.recv() => l,
        _ = tokio::signal::ctrl_c() => {
            handle.cancel();
            None
        }
    };
    // EOF / interrupt count as a denial: never auto-approve.
    let yes = answer
        .as_deref()
        .is_some_and(|a| matches!(a.trim().to_ascii_lowercase().as_str(), "y" | "yes"));
    handle.respond(&req.approval_id, yes, None);
}

/// Prompt and read one line; Ctrl-C at the prompt exits.
async fn read_prompt(lines: &mut UnboundedReceiver<String>) -> Option<String> {
    out("> ");
    tokio::select! {
        l = lines.recv() => l,
        _ = tokio::signal::ctrl_c() => {
            out("\n");
            None
        }
    }
}

/// Blocking stdin lives on its own thread so one reader serves both the REPL and approvals.
fn stdin_lines() -> UnboundedReceiver<String> {
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

/// Errors that never reached `tools/post-execute` (policy denials, bad args) would otherwise
/// be invisible live; show them from the log.
fn report_unshown(events: &[hk_session::SessionEvent], ui: &Ui) {
    let shown = ui.shown().clone();
    for ev in events {
        if let EventKind::ToolResult {
            call_id,
            content,
            is_error: true,
        } = &ev.kind
            && !shown.contains(call_id)
        {
            err(&format!("  = error: {}\n", clip(content, 300)));
        }
    }
}

fn report_turn(s: &TurnSummary) {
    err(&dim(&format!(
        "[{:?} | {} steps | {} in / {} out tokens]\n",
        s.reason,
        s.steps,
        s.usage.total_input(),
        s.usage.output_tokens
    )));
    if let Some(e) = &s.error {
        err(&format!("error: {e}\n"));
    }
}
