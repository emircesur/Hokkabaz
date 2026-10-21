//! Compaction planning: when to summarize and where to cut.
//!
//! The summary is a `compaction/summary` log event; `derive_messages` drops non-system
//! messages with `seq <= through_seq` and shows the summary instead, so the log stays the
//! source of truth and replay needs nothing from this module. The summarization request itself
//! is `derive_messages(log[..cut])` + [`COMPACT_PROMPT`], both recoverable from the log.
// port: dsh packages/compaction (threshold trigger, summary replaces older turns)

use hk_llm::{Message, Role};
use hk_session::{EventKind, SessionEvent, derive_messages};

/// Final user turn of the summarization request.
pub(crate) const COMPACT_PROMPT: &str = "Summarize the conversation so far so work can continue from the summary alone. \
Keep: the user's goals and constraints, decisions made, files and commands touched with their \
outcomes, errors hit, and what is still to do. Drop pleasantries and raw tool output. \
Reply with the summary only.";

/// Prefixed to the model's summary so the log and the prompt show the same text.
pub(crate) const SUMMARY_PREFIX: &str =
    "Summary of the earlier conversation (older messages were compacted):\n\n";

/// Where to cut: `log[..index]` is summarized, `log[index..]` stays verbatim.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Cut {
    pub index: usize,
    pub through_seq: u64,
}

/// Rough token count (4 chars per token plus per-message framing). Providers report exact usage
/// after the fact; this only has to be monotonic with context size.
pub(crate) fn estimate_tokens(msgs: &[Message]) -> u64 {
    msgs.iter()
        .map(|m| {
            let calls: usize = m
                .tool_calls
                .iter()
                .map(|c| c.name.len() + c.arguments.to_string().len())
                .sum();
            let reasoning = m.reasoning.as_ref().map_or(0, String::len);
            ((m.content.len() + calls + reasoning) as u64).div_ceil(4) + 4
        })
        .sum()
}

/// Current context size: the larger of the estimate over the derived history and the prompt
/// size the provider reported for the latest request since the last compaction (it also counts
/// tool schemas, which the estimate does not).
pub(crate) fn context_tokens(events: &[SessionEvent]) -> u64 {
    let est = estimate_tokens(&derive_messages(events));
    let since = last_compaction(events).map_or(0, |i| i + 1);
    let reported = events[since..]
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EventKind::StepEnd { usage: Some(u), .. } => Some(u.total_input()),
            _ => None,
        })
        .unwrap_or(0);
    est.max(reported)
}

fn last_compaction(events: &[SessionEvent]) -> Option<usize> {
    events
        .iter()
        .rposition(|e| matches!(e.kind, EventKind::CompactionSummary { .. }))
}

/// Pick the cut that keeps the last `keep_recent_steps` steps verbatim.
///
/// Cuts sit just before a `step/start` (every earlier call is answered there), pulled back over
/// the user messages logged for that step and the `turn/start` that owns them. Only steps after
/// the latest compaction qualify: its summary event sits at the log's end, so a cut behind it
/// would leave that summary in place beside the new one.
pub(crate) fn plan_cut(events: &[SessionEvent], keep_recent_steps: usize) -> Option<Cut> {
    let keep = keep_recent_steps.max(1);
    let from = last_compaction(events).map_or(0, |i| i + 1);
    let steps: Vec<usize> = events
        .iter()
        .enumerate()
        .skip(from)
        .filter(|(_, e)| matches!(e.kind, EventKind::StepStart { .. }))
        .map(|(i, _)| i)
        .collect();
    if steps.len() <= keep {
        return None;
    }
    let mut c = steps[steps.len() - keep];
    while c > from && matches!(events[c - 1].kind, EventKind::UserMessage { .. }) {
        c -= 1;
    }
    if c > from && matches!(events[c - 1].kind, EventKind::TurnStart { .. }) {
        c -= 1;
    }
    // Nothing model-visible before the cut: a summary would summarize nothing.
    let has_content = events[from.min(c)..c].iter().any(|e| {
        matches!(
            e.kind,
            EventKind::UserMessage { .. }
                | EventKind::AssistantMessage { .. }
                | EventKind::ToolResult { .. }
        )
    });
    if c == 0 || !has_content {
        return None;
    }
    Some(Cut {
        index: c,
        through_seq: events[c - 1].seq,
    })
}

pub(crate) fn user(text: &str) -> Message {
    Message {
        role: Role::User,
        content: text.to_owned(),
        reasoning: None,
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}
