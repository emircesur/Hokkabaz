//! `hk`: Hokkabaz CLI.

mod acp_chat;
mod auth;
mod chat;
mod paths;
mod sessions;
mod term;
mod tui;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use std::io::IsTerminal;
use hk_session::SessionStore;

#[derive(Debug, Parser)]
#[command(name = "hk", version, about = "Hokkabaz agent harness CLI")]
struct Cli {
    /// Data directory (sessions, db). Default: ~/.hokkabaz.
    #[arg(long, global = true, env = "HOKKABAZ_HOME")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Chat with the agent (REPL, or one-shot with a prompt).
    Chat(chat::ChatArgs),
    /// Full-screen terminal UI (default when run bare in a terminal).
    Tui(tui::TuiArgs),
    /// Inspect stored sessions.
    Sessions {
        #[command(subcommand)]
        cmd: SessionsCmd,
    },
    /// Manage provider credentials.
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },
}

#[derive(Debug, Subcommand)]
enum SessionsCmd {
    /// List sessions, newest first.
    Ls {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Print a session transcript.
    Show {
        /// Session id or unique prefix.
        id: String,
    },
}

#[derive(Debug, Subcommand)]
enum AuthCmd {
    /// Store the DeepSeek API key in the OS keyring (read from stdin).
    Set,
}

fn main() -> Result<()> {
    // Landlock helper re-exec: must run before anything else touches the process.
    hk_sandbox::maybe_run_helper();
    let cli = Cli::parse();
    let cmd = match cli.cmd {
        Some(c) => c,
        None if std::io::stdout().is_terminal() && std::io::stdin().is_terminal() => {
            Cmd::Tui(tui::TuiArgs::default())
        }
        None => {
            let _ = Cli::command().print_help();
            return Ok(());
        }
    };
    let paths = paths::Paths::resolve(cli.data_dir)?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?
        .block_on(async move {
            match cmd {
                Cmd::Chat(args) => chat::run(&paths, args).await,
                Cmd::Tui(args) => tui::run(&paths, args).await,
                Cmd::Sessions { cmd } => {
                    let store = SessionStore::open(paths.sessions()).context("open session store")?;
                    match cmd {
                        SessionsCmd::Ls { limit } => sessions::ls(&store, limit),
                        SessionsCmd::Show { id } => sessions::show(&store, &id),
                    }
                }
                Cmd::Auth { cmd: AuthCmd::Set } => auth::set(&paths).await,
            }
        })
}
