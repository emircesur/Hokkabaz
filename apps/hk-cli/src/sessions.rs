//! `hk sessions ls|show`.

use anyhow::{Result, bail};
use hk_session::{EventKind, SessionStore};

use crate::term::{clip, fmt_ts, out};

pub fn ls(store: &SessionStore, limit: u32) -> Result<()> {
    let list = store.index().list(limit, 0)?;
    if list.is_empty() {
        out("no sessions\n");
        return Ok(());
    }
    for s in list {
        out(&format!(
            "{}  {}  {:>3} turns  {:<18}  {}\n",
            s.id,
            fmt_ts(s.updated_ms),
            s.turn_count,
            s.model.as_deref().unwrap_or("-"),
            clip(s.title.as_deref().unwrap_or(""), 60),
        ));
    }
    Ok(())
}

/// Unique id prefix -> full id, so users can type the short form from `ls`.
pub fn resolve(store: &SessionStore, prefix: &str) -> Result<String> {
    let all = store.index().list(10_000, 0)?;
    if all.iter().any(|s| s.id == prefix) {
        return Ok(prefix.to_owned());
    }
    let hits: Vec<_> = all.iter().filter(|s| s.id.starts_with(prefix)).collect();
    match hits.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => bail!("no session matches `{prefix}`"),
        _ => bail!("`{prefix}` is ambiguous ({} sessions match)", hits.len()),
    }
}

pub fn latest(store: &SessionStore) -> Result<String> {
    match store.index().list(1, 0)?.into_iter().next() {
        Some(s) => Ok(s.id),
        None => bail!("no sessions to resume"),
    }
}

pub fn show(store: &SessionStore, prefix: &str) -> Result<()> {
    let id = resolve(store, prefix)?;
    let events = store.read(&id)?;
    for ev in &events {
        match &ev.kind {
            EventKind::SessionStart {
                cwd, model, title, ..
            } => out(&format!(
                "session {id}\nmodel {}  cwd {}  title {}\n\n",
                model.as_deref().unwrap_or("-"),
                cwd.as_deref().unwrap_or("-"),
                title.as_deref().unwrap_or("-"),
            )),
            EventKind::SessionTitle { title } => out(&format!("-- title: {title}\n")),
            EventKind::UserMessage { content } => out(&format!("you> {content}\n")),
            EventKind::AssistantMessage { content, .. } if !content.is_empty() => {
                out(&format!("{content}\n"));
            }
            EventKind::ToolCall {
                name, arguments, ..
            } => out(&format!("  > {name} {}\n", clip(&arguments.to_string(), 200))),
            EventKind::ToolResult {
                content, is_error, ..
            } => {
                let tag = if *is_error { "error" } else { "result" };
                out(&format!("  = {tag}: {}\n", clip(&one_line(content), 200)));
            }
            EventKind::ApprovalRequest { tool, detail, .. } => out(&format!(
                "  ? approval for {tool} {}\n",
                detail.as_deref().unwrap_or("")
            )),
            EventKind::ApprovalResponse {
                approved, reason, ..
            } => out(&format!(
                "  ? {}{}\n",
                if *approved { "approved" } else { "denied" },
                reason.as_deref().map(|r| format!(" ({r})")).unwrap_or_default()
            )),
            EventKind::TurnEnd { reason, .. } => out(&format!("-- turn {reason:?}\n\n")),
            _ => {}
        }
    }
    Ok(())
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
