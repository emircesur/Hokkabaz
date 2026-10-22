//! Pure TUI state + reducer. No terminal, no runtime: `update(Msg) -> Vec<Cmd>` so the whole
//! thing is unit-testable. `mod.rs` turns terminal/runtime events into [`Msg`] and runs [`Cmd`]s.
//!
//! Two feeds drive the transcript: logged `SessionEvent`s (authoritative, in order, tailed
//! every ~250ms by the runtime) and live `StreamEvent` deltas (never logged). Deltas live in a
//! separate `live` buffer keyed by (turn, step) and are dropped once the log catches up, so a
//! slow log tail can never reorder text behind tool cards.

use std::cell::Cell;
use std::collections::VecDeque;

use hk_llm::{ModelInfo, StreamEvent, Usage};
use hk_server::{ServerEvent, SessionInfo, TurnMode, TurnRequest};
use hk_session::{EventKind, SessionEvent, TurnEndReason};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

/// Which pane gets navigation keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    Sessions,
}

/// Runtime feed health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conn {
    Live,
    /// Fell behind the broadcast; resyncing from the log.
    Lagged,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    Models { sel: usize },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCard {
    pub call_id: String,
    pub name: String,
    pub args: Value,
    /// `(content, is_error)` once the result is logged.
    pub result: Option<(String, bool)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    User(String),
    Assistant {
        text: String,
        reasoning: String,
    },
    Tool(ToolCard),
    /// Errors, turn markers.
    Note {
        text: String,
        error: bool,
    },
}

/// Streaming text of the step in flight.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Live {
    pub key: (String, u32),
    pub text: String,
    pub reasoning: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    pub id: String,
    pub call_id: String,
    pub tool: String,
    pub detail: Option<String>,
}

/// Multi-line edit buffer; `cursor` is a byte index on a char boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    pub text: String,
    pub cursor: usize,
}

impl Input {
    pub fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    pub fn backspace(&mut self) {
        if let Some(p) = self.prev() {
            self.text.replace_range(p..self.cursor, "");
            self.cursor = p;
        }
    }

    pub fn delete(&mut self) {
        if let Some(n) = self.next() {
            self.text.replace_range(self.cursor..n, "");
        }
    }

    fn prev(&self) -> Option<usize> {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
    }

    fn next(&self) -> Option<usize> {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
    }

    pub fn left(&mut self) {
        if let Some(p) = self.prev() {
            self.cursor = p;
        }
    }

    pub fn right(&mut self) {
        if let Some(n) = self.next() {
            self.cursor = n;
        }
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    /// Move a line up/down keeping the char column where possible.
    pub fn vertical(&mut self, down: bool) {
        let start = self.line_start();
        let col = self.text[start..self.cursor].chars().count();
        let target_start = if down {
            match self.text[self.line_end()..].strip_prefix('\n') {
                Some(_) => self.line_end() + 1,
                None => return,
            }
        } else {
            if start == 0 {
                return;
            }
            self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1)
        };
        let line_end = self.text[target_start..]
            .find('\n')
            .map_or(self.text.len(), |i| target_start + i);
        let line = &self.text[target_start..line_end];
        let off = line.char_indices().nth(col).map_or(line.len(), |(i, _)| i);
        self.cursor = target_start + off;
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }
}

/// Everything that can happen to the UI.
#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    /// Terminal rows; used for the PgUp/PgDn step.
    Resize {
        height: u16,
    },
    Stream {
        turn_id: String,
        step: u32,
        event: StreamEvent,
    },
    Log(Box<SessionEvent>),
    /// Runtime-reported failure for the session.
    SessionError(String),
    /// Async command failed.
    Error(String),
    Sessions(Vec<SessionInfo>),
    Models(Vec<ModelInfo>),
    Created(Box<SessionInfo>),
    Loaded {
        session_id: String,
        events: Vec<SessionEvent>,
        pending: Vec<String>,
    },
    Conn(Conn),
}

/// Side effects the driver must run.
#[derive(Debug)]
pub enum Cmd {
    Submit {
        session_id: String,
        req: TurnRequest,
    },
    Cancel(String),
    Respond {
        approval_id: String,
        session_id: String,
        approved: bool,
        reason: Option<String>,
    },
    CreateSession {
        model: String,
    },
    Load(String),
    RefreshSessions,
    Quit,
}

impl Msg {
    /// Map a runtime feed event for the current session; `None` for events the UI ignores.
    pub fn from_feed(ev: &ServerEvent) -> Option<Msg> {
        match ev.name.as_str() {
            "agent/assistant-stream" => {
                let turn_id = ev.data.get("turn_id")?.as_str()?.to_owned();
                let step = u32::try_from(ev.data.get("step")?.as_u64()?).ok()?;
                let event = serde_json::from_value(ev.data.get("event")?.clone()).ok()?;
                Some(Msg::Stream {
                    turn_id,
                    step,
                    event,
                })
            }
            "session/error" => Some(Msg::SessionError(
                ev.data
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_owned(),
            )),
            _ => serde_json::from_value::<SessionEvent>(ev.data.clone())
                .ok()
                .map(|e| Msg::Log(Box::new(e))),
        }
    }
}

#[derive(Debug)]
pub struct TuiState {
    pub sessions: Vec<SessionInfo>,
    /// Highlighted row in the sessions pane.
    pub sel: usize,
    pub current: Option<String>,
    pub focus: Focus,
    pub blocks: Vec<Block>,
    pub live: Option<Live>,
    log_step: Option<(String, u32)>,
    last_seq: Option<u64>,
    /// Log events that arrived while the history snapshot was loading.
    loading: bool,
    backlog: Vec<SessionEvent>,
    pub running: bool,
    /// Sum of logged step usage.
    pub usage: Usage,
    pub mode: TurnMode,
    pub model: String,
    pub models: Vec<ModelInfo>,
    pub input: Input,
    /// Lines scrolled up from the bottom; 0 follows the stream.
    pub scroll: usize,
    /// Set by the renderer so PgUp can't run past the top.
    pub scroll_max: Cell<usize>,
    page: usize,
    pub show_reasoning: bool,
    pub expand_tools: bool,
    pub overlay: Overlay,
    pub approvals: VecDeque<Approval>,
    /// `Some` while the approval modal collects a denial reason.
    pub reason: Option<String>,
    pub conn: Conn,
    quit_armed: bool,
    pub flash: Option<String>,
    /// Prompt typed before any session existed; sent once creation lands.
    pending_prompt: Option<String>,
}

impl TuiState {
    pub fn new(model: String, mode: TurnMode) -> Self {
        Self {
            sessions: Vec::new(),
            sel: 0,
            current: None,
            focus: Focus::Input,
            blocks: Vec::new(),
            live: None,
            log_step: None,
            last_seq: None,
            loading: false,
            backlog: Vec::new(),
            running: false,
            usage: Usage::default(),
            mode,
            model,
            models: Vec::new(),
            input: Input::default(),
            scroll: 0,
            scroll_max: Cell::new(0),
            page: 10,
            show_reasoning: false,
            expand_tools: false,
            overlay: Overlay::None,
            approvals: VecDeque::new(),
            reason: None,
            conn: Conn::Live,
            quit_armed: false,
            flash: None,
            pending_prompt: None,
        }
    }

    pub fn quit_armed(&self) -> bool {
        self.quit_armed
    }

    /// Mark `id` current and clear everything derived from the previous session's log.
    pub fn begin_session(&mut self, id: &str, loading: bool) {
        self.current = Some(id.to_owned());
        self.blocks.clear();
        self.live = None;
        self.log_step = None;
        self.last_seq = None;
        self.loading = loading;
        self.backlog.clear();
        self.running = false;
        self.usage = Usage::default();
        self.approvals.clear();
        self.reason = None;
        self.scroll = 0;
        if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
            self.sel = i;
        }
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Cmd> {
        match msg {
            Msg::Key(k) => self.key(k),
            Msg::Paste(s) => {
                if self.accepts_text() {
                    self.input.insert(&s.replace('\r', ""));
                }
                Vec::new()
            }
            Msg::Resize { height } => {
                self.page = usize::from(height.saturating_sub(8)).max(3);
                Vec::new()
            }
            Msg::Stream {
                turn_id,
                step,
                event,
            } => {
                self.stream(turn_id, step, event);
                Vec::new()
            }
            Msg::Log(ev) => {
                if self.loading {
                    self.backlog.push(*ev);
                    Vec::new()
                } else {
                    self.log(*ev)
                }
            }
            Msg::SessionError(m) => {
                self.blocks.push(Block::Note {
                    text: format!("error: {m}"),
                    error: true,
                });
                self.running = false;
                self.live = None;
                Vec::new()
            }
            Msg::Error(m) => {
                self.flash = Some(m);
                Vec::new()
            }
            Msg::Sessions(list) => {
                self.sessions = list;
                self.sel = self
                    .current
                    .as_ref()
                    .and_then(|c| self.sessions.iter().position(|s| &s.id == c))
                    .unwrap_or(self.sel)
                    .min(self.sessions.len().saturating_sub(1));
                Vec::new()
            }
            Msg::Models(m) => {
                self.models = m;
                Vec::new()
            }
            Msg::Created(info) => {
                let id = info.id.clone();
                self.sessions.insert(0, *info);
                self.begin_session(&id, false);
                match self.pending_prompt.take() {
                    Some(p) => vec![self.submit_cmd(id, p)],
                    None => Vec::new(),
                }
            }
            Msg::Loaded {
                session_id,
                events,
                pending,
            } => {
                if self.current.as_deref() != Some(&session_id) {
                    return Vec::new();
                }
                let mut cmds = Vec::new();
                self.loading = false;
                for ev in events.into_iter().chain(std::mem::take(&mut self.backlog)) {
                    cmds.extend(self.log(ev));
                }
                // Replay re-queues approvals that were settled in another process.
                self.approvals.retain(|a| pending.contains(&a.id));
                cmds
            }
            Msg::Conn(c) => {
                self.conn = c;
                match (c, self.current.clone()) {
                    // Missed deltas are fine; missed log events are not: reload.
                    (Conn::Lagged, Some(id)) => {
                        self.begin_session(&id, true);
                        vec![Cmd::Load(id)]
                    }
                    _ => Vec::new(),
                }
            }
        }
    }

    fn accepts_text(&self) -> bool {
        self.overlay == Overlay::None && self.focus == Focus::Input && self.approvals.is_empty()
    }

    fn submit_cmd(&self, session_id: String, prompt: String) -> Cmd {
        Cmd::Submit {
            session_id,
            req: TurnRequest {
                prompt,
                mode: Some(self.mode),
                model: Some(self.model.clone()).filter(|m| !m.is_empty()),
            },
        }
    }

    // ---- feeds ----

    fn stream(&mut self, turn_id: String, step: u32, event: StreamEvent) {
        let key = (turn_id, step);
        let (is_text, s) = match event {
            StreamEvent::TextDelta { text } => (true, text),
            StreamEvent::ReasoningDelta { text } => (false, text),
            // Tool calls and usage show up from the log; Finish only ends the step's stream.
            _ => return,
        };
        let live = match &mut self.live {
            Some(l) if l.key == key => l,
            slot => slot.insert(Live {
                key,
                ..Live::default()
            }),
        };
        if is_text {
            &mut live.text
        } else {
            &mut live.reasoning
        }
        .push_str(&s);
    }

    fn log(&mut self, ev: SessionEvent) -> Vec<Cmd> {
        if self.last_seq.is_some_and(|l| ev.seq <= l) {
            return Vec::new();
        }
        self.last_seq = Some(ev.seq);
        let mut cmds = Vec::new();
        match ev.kind {
            EventKind::SessionTitle { title } => {
                if let Some(s) = self
                    .sessions
                    .iter_mut()
                    .find(|s| Some(&s.id) == self.current.as_ref())
                {
                    s.title = Some(title);
                }
            }
            EventKind::TurnStart { .. } => self.running = true,
            EventKind::TurnEnd { reason, .. } => {
                self.running = false;
                self.live = None;
                match reason {
                    TurnEndReason::Completed => {}
                    r => self.blocks.push(Block::Note {
                        text: format!("turn {r:?}"),
                        error: r == TurnEndReason::Failed,
                    }),
                }
                cmds.push(Cmd::RefreshSessions);
            }
            EventKind::StepStart { turn_id, step } => self.log_step = Some((turn_id, step)),
            EventKind::StepEnd { usage, .. } => {
                if let Some(u) = usage {
                    self.usage.accumulate(&u);
                }
                if self
                    .live
                    .as_ref()
                    .is_some_and(|l| Some(&l.key) == self.log_step.as_ref())
                {
                    self.live = None;
                }
            }
            EventKind::UserMessage { content } => self.blocks.push(Block::User(content)),
            EventKind::AssistantMessage { content, reasoning } => {
                if self
                    .live
                    .as_ref()
                    .is_some_and(|l| Some(&l.key) == self.log_step.as_ref())
                {
                    self.live = None;
                }
                let reasoning = reasoning.unwrap_or_default();
                if !content.is_empty() || !reasoning.is_empty() {
                    self.blocks.push(Block::Assistant {
                        text: content,
                        reasoning,
                    });
                }
            }
            EventKind::AssistantAttempt {
                attempt,
                error: Some(e),
                ..
            } => {
                self.blocks.push(Block::Note {
                    text: format!("attempt {attempt} failed: {e}"),
                    error: true,
                });
            }
            EventKind::CompactionSummary { .. } => {
                self.blocks.push(Block::Note {
                    text: "context compacted".into(),
                    error: false,
                });
            }
            EventKind::ToolCall {
                call_id,
                name,
                arguments,
            } => {
                self.blocks.push(Block::Tool(ToolCard {
                    call_id,
                    name,
                    args: arguments,
                    result: None,
                }));
            }
            EventKind::ToolResult {
                call_id,
                content,
                is_error,
            } => {
                let card = self.blocks.iter_mut().rev().find_map(|b| match b {
                    Block::Tool(c) if c.call_id == call_id => Some(c),
                    _ => None,
                });
                match card {
                    Some(c) => c.result = Some((content, is_error)),
                    None => self.blocks.push(Block::Note {
                        text: content,
                        error: is_error,
                    }),
                }
            }
            EventKind::ApprovalRequest {
                approval_id,
                call_id,
                tool,
                detail,
            } => {
                if !self.approvals.iter().any(|a| a.id == approval_id) {
                    self.approvals.push_back(Approval {
                        id: approval_id,
                        call_id,
                        tool,
                        detail,
                    });
                }
            }
            EventKind::ApprovalResponse { approval_id, .. } => {
                self.approvals.retain(|a| a.id != approval_id);
                if self.approvals.is_empty() {
                    self.reason = None;
                }
            }
            _ => {}
        }
        cmds
    }

    // ---- keys ----

    fn key(&mut self, k: KeyEvent) -> Vec<Cmd> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let ch = match k.code {
            KeyCode::Char(c) => Some(c.to_ascii_lowercase()),
            _ => None,
        };
        let is_ctrl_c = ctrl && ch == Some('c');
        if !is_ctrl_c {
            self.quit_armed = false;
        }
        self.flash = None;

        if ctrl && ch == Some('q') {
            return self.quit();
        }
        if is_ctrl_c {
            if self.quit_armed {
                return self.quit();
            }
            self.quit_armed = true;
            self.flash = Some("press Ctrl+C again to quit".into());
            return Vec::new();
        }
        if !self.approvals.is_empty() {
            return self.approval_key(k.code);
        }
        match self.overlay.clone() {
            Overlay::Help => {
                self.overlay = Overlay::None;
                return Vec::new();
            }
            Overlay::Models { sel } => {
                self.model_key(k.code, sel);
                return Vec::new();
            }
            Overlay::None => {}
        }

        // Globals.
        match k.code {
            KeyCode::F(1) => {
                self.overlay = Overlay::Help;
                return Vec::new();
            }
            KeyCode::PageUp => {
                self.scroll = (self.scroll + self.page).min(self.scroll_max.get());
                return Vec::new();
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(self.page);
                return Vec::new();
            }
            _ => {}
        }
        if ctrl {
            match ch {
                // Ctrl+M is Enter on legacy terminals; Ctrl+O is the reliable twin.
                Some('m' | 'o') => {
                    let sel = self
                        .models
                        .iter()
                        .position(|m| m.id == self.model)
                        .unwrap_or(0);
                    if self.models.is_empty() {
                        self.flash = Some("no models loaded".into());
                    } else {
                        self.overlay = Overlay::Models { sel };
                    }
                    return Vec::new();
                }
                Some('r') => {
                    self.show_reasoning = !self.show_reasoning;
                    return Vec::new();
                }
                Some('e') => {
                    self.expand_tools = !self.expand_tools;
                    return Vec::new();
                }
                Some('b') => {
                    self.focus = if self.focus == Focus::Input {
                        Focus::Sessions
                    } else {
                        Focus::Input
                    };
                    return Vec::new();
                }
                Some('n') => return self.new_session(),
                _ => {}
            }
        }

        match self.focus {
            Focus::Sessions => self.sessions_key(k.code),
            Focus::Input => self.input_key(k, ctrl, alt, ch),
        }
    }

    fn quit(&mut self) -> Vec<Cmd> {
        let mut v = Vec::new();
        if let (true, Some(id)) = (self.running, &self.current) {
            v.push(Cmd::Cancel(id.clone()));
        }
        v.push(Cmd::Quit);
        v
    }

    fn new_session(&mut self) -> Vec<Cmd> {
        self.focus = Focus::Input;
        vec![Cmd::CreateSession {
            model: self.model.clone(),
        }]
    }

    fn approval_key(&mut self, code: KeyCode) -> Vec<Cmd> {
        let Some(front) = self.approvals.front().cloned() else {
            return Vec::new();
        };
        let Some(session_id) = self.current.clone() else {
            return Vec::new();
        };
        let respond = |approved, reason| Cmd::Respond {
            approval_id: front.id.clone(),
            session_id: session_id.clone(),
            approved,
            reason,
        };
        if let Some(r) = &mut self.reason {
            return match code {
                KeyCode::Esc => {
                    self.reason = None;
                    Vec::new()
                }
                KeyCode::Enter => {
                    let reason = std::mem::take(r);
                    self.reason = None;
                    self.approvals.pop_front();
                    vec![respond(
                        false,
                        Some(reason).filter(|r| !r.trim().is_empty()),
                    )]
                }
                KeyCode::Backspace => {
                    r.pop();
                    Vec::new()
                }
                KeyCode::Char(c) => {
                    r.push(c);
                    Vec::new()
                }
                _ => Vec::new(),
            };
        }
        match code {
            KeyCode::Char('y' | 'Y') => {
                self.approvals.pop_front();
                vec![respond(true, None)]
            }
            // Esc denies: never approve by accident.
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.approvals.pop_front();
                vec![respond(false, None)]
            }
            KeyCode::Char('r' | 'R') => {
                self.reason = Some(String::new());
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn model_key(&mut self, code: KeyCode, sel: usize) {
        let n = self.models.len();
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = Overlay::None,
            KeyCode::Up | KeyCode::Char('k') => {
                self.overlay = Overlay::Models {
                    sel: sel.saturating_sub(1),
                };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.overlay = Overlay::Models {
                    sel: (sel + 1).min(n.saturating_sub(1)),
                };
            }
            KeyCode::Enter => {
                if let Some(m) = self.models.get(sel) {
                    self.model = m.id.clone();
                    self.flash = Some(format!("model: {}", m.id));
                }
                self.overlay = Overlay::None;
            }
            _ => {}
        }
    }

    fn sessions_key(&mut self, code: KeyCode) -> Vec<Cmd> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel = (self.sel + 1).min(self.sessions.len().saturating_sub(1));
            }
            KeyCode::Char('n') => return self.new_session(),
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            KeyCode::Esc | KeyCode::Tab => self.focus = Focus::Input,
            KeyCode::Enter | KeyCode::Right => {
                if let Some(s) = self.sessions.get(self.sel).cloned() {
                    self.focus = Focus::Input;
                    if self.current.as_deref() != Some(&s.id) {
                        self.begin_session(&s.id, true);
                        if let Some(m) = &s.model {
                            self.model.clone_from(m);
                        }
                        return vec![Cmd::Load(s.id)];
                    }
                }
            }
            _ => {}
        }
        Vec::new()
    }

    fn input_key(&mut self, k: KeyEvent, ctrl: bool, alt: bool, ch: Option<char>) -> Vec<Cmd> {
        match k.code {
            KeyCode::Enter if ctrl || alt => return self.send(),
            KeyCode::Char(_) if ctrl && ch == Some('s') => return self.send(),
            KeyCode::Enter => self.input.insert("\n"),
            KeyCode::Tab => {
                self.mode = match self.mode {
                    TurnMode::Build => TurnMode::Plan,
                    TurnMode::Plan => TurnMode::Build,
                };
            }
            KeyCode::Esc => {
                if let (true, Some(id)) = (self.running, &self.current) {
                    self.flash = Some("cancelling turn".into());
                    return vec![Cmd::Cancel(id.clone())];
                }
            }
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Up => self.input.vertical(false),
            KeyCode::Down => self.input.vertical(true),
            KeyCode::Home => self.input.home(),
            KeyCode::End => {
                if self.input.text.is_empty() {
                    self.scroll = 0;
                } else {
                    self.input.end();
                }
            }
            KeyCode::Char('?') if self.input.text.is_empty() => self.overlay = Overlay::Help,
            KeyCode::Char(c) if !ctrl => {
                let mut buf = [0u8; 4];
                self.input.insert(c.encode_utf8(&mut buf));
            }
            _ => {}
        }
        Vec::new()
    }

    fn send(&mut self) -> Vec<Cmd> {
        if self.input.text.trim().is_empty() {
            return Vec::new();
        }
        let prompt = self.input.take();
        self.scroll = 0;
        match self.current.clone() {
            Some(id) => vec![self.submit_cmd(id, prompt)],
            None => {
                self.pending_prompt = Some(prompt);
                vec![Cmd::CreateSession {
                    model: self.model.clone(),
                }]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use hk_session::SessionEvent;

    use super::*;

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(c: char) -> Msg {
        Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn ev(seq: u64, kind: EventKind) -> Msg {
        Msg::Log(Box::new(SessionEvent {
            seq,
            ts_ms: 0,
            kind,
        }))
    }

    fn info(id: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            title: None,
            model: None,
            cwd: None,
            created_ms: 0,
            updated_ms: 0,
            event_count: 0,
            turn_count: 0,
            running: false,
        }
    }

    fn state() -> TuiState {
        let mut s = TuiState::new("deepseek-chat".into(), TurnMode::Build);
        s.begin_session("s1", false);
        s
    }

    fn typed(s: &mut TuiState, text: &str) {
        for c in text.chars() {
            s.update(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn stream_then_log_replaces_live_buffer() {
        let mut s = state();
        s.update(ev(
            1,
            EventKind::StepStart {
                turn_id: "t".into(),
                step: 0,
            },
        ));
        s.update(Msg::Stream {
            turn_id: "t".into(),
            step: 0,
            event: StreamEvent::TextDelta { text: "hel".into() },
        });
        s.update(Msg::Stream {
            turn_id: "t".into(),
            step: 0,
            event: StreamEvent::TextDelta { text: "lo".into() },
        });
        assert_eq!(s.live.as_ref().unwrap().text, "hello");
        s.update(ev(
            2,
            EventKind::AssistantMessage {
                content: "hello".into(),
                reasoning: None,
            },
        ));
        assert!(s.live.is_none());
        assert_eq!(
            s.blocks,
            vec![Block::Assistant {
                text: "hello".into(),
                reasoning: String::new()
            }]
        );
    }

    #[test]
    fn late_log_does_not_drop_next_step_live_text() {
        let mut s = state();
        s.update(ev(
            1,
            EventKind::StepStart {
                turn_id: "t".into(),
                step: 0,
            },
        ));
        // Step 1 deltas arrive before step 0's assistant message is tailed from the log.
        s.update(Msg::Stream {
            turn_id: "t".into(),
            step: 1,
            event: StreamEvent::TextDelta {
                text: "next".into(),
            },
        });
        s.update(ev(
            2,
            EventKind::AssistantMessage {
                content: "first".into(),
                reasoning: None,
            },
        ));
        assert_eq!(s.live.as_ref().unwrap().text, "next");
        assert_eq!(s.blocks.len(), 1);
    }

    #[test]
    fn log_events_dedupe_by_seq_and_tool_result_attaches() {
        let mut s = state();
        let call = EventKind::ToolCall {
            call_id: "c1".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path": "a"}),
        };
        s.update(ev(3, call.clone()));
        s.update(ev(3, call));
        s.update(ev(
            4,
            EventKind::ToolResult {
                call_id: "c1".into(),
                content: "ok".into(),
                is_error: false,
            },
        ));
        assert_eq!(s.blocks.len(), 1);
        let Block::Tool(c) = &s.blocks[0] else {
            panic!("tool card")
        };
        assert_eq!(c.result, Some(("ok".into(), false)));
    }

    #[test]
    fn turn_lifecycle_and_usage() {
        let mut s = state();
        s.update(ev(
            1,
            EventKind::TurnStart {
                turn_id: "t".into(),
            },
        ));
        assert!(s.running);
        s.update(ev(
            2,
            EventKind::StepEnd {
                turn_id: "t".into(),
                step: 0,
                finish: None,
                usage: Some(Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Usage::default()
                }),
            },
        ));
        let cmds = s.update(ev(
            3,
            EventKind::TurnEnd {
                turn_id: "t".into(),
                reason: TurnEndReason::Cancelled,
            },
        ));
        assert!(!s.running);
        assert_eq!(s.usage.total(), 15);
        assert!(matches!(cmds.as_slice(), [Cmd::RefreshSessions]));
        assert!(matches!(s.blocks.last(), Some(Block::Note { .. })));
    }

    #[test]
    fn approval_modal_y_n_reason() {
        let mut s = state();
        let req = |n: u64, id: &str| {
            ev(
                n,
                EventKind::ApprovalRequest {
                    approval_id: id.into(),
                    call_id: "c".into(),
                    tool: "bash".into(),
                    detail: Some("ls".into()),
                },
            )
        };
        s.update(req(1, "a1"));
        s.update(req(2, "a2"));
        s.update(req(3, "a3"));
        assert_eq!(s.approvals.len(), 3);
        // Typing is swallowed by the modal.
        s.update(key(KeyCode::Char('x')));
        assert!(s.input.text.is_empty());

        let cmds = s.update(key(KeyCode::Char('y')));
        assert!(
            matches!(&cmds[..], [Cmd::Respond { approval_id, approved: true, reason: None, .. }] if approval_id == "a1")
        );

        s.update(key(KeyCode::Char('r')));
        typed(&mut s, "no way");
        let cmds = s.update(key(KeyCode::Enter));
        assert!(matches!(&cmds[..],
            [Cmd::Respond { approval_id, approved: false, reason: Some(r), .. }] if approval_id == "a2" && r == "no way"));

        let cmds = s.update(key(KeyCode::Esc));
        assert!(matches!(
            &cmds[..],
            [Cmd::Respond {
                approved: false,
                ..
            }]
        ));
        assert!(s.approvals.is_empty());
    }

    #[test]
    fn approval_response_in_log_clears_modal() {
        let mut s = state();
        s.update(ev(
            1,
            EventKind::ApprovalRequest {
                approval_id: "a".into(),
                call_id: "c".into(),
                tool: "bash".into(),
                detail: None,
            },
        ));
        s.update(ev(
            2,
            EventKind::ApprovalResponse {
                approval_id: "a".into(),
                approved: true,
                reason: None,
            },
        ));
        assert!(s.approvals.is_empty());
    }

    #[test]
    fn send_keys_and_multiline_input() {
        let mut s = state();
        typed(&mut s, "ab");
        s.update(key(KeyCode::Enter));
        typed(&mut s, "cd");
        assert_eq!(s.input.text, "ab\ncd");
        s.update(key(KeyCode::Up));
        s.update(key(KeyCode::Char('X')));
        assert_eq!(s.input.text, "abX\ncd");
        let cmds = s.update(Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)));
        match &cmds[..] {
            [Cmd::Submit { session_id, req }] => {
                assert_eq!(session_id, "s1");
                assert_eq!(req.prompt, "abX\ncd");
                assert_eq!(req.mode, Some(TurnMode::Build));
            }
            other => panic!("{other:?}"),
        }
        assert!(s.input.text.is_empty());
        // Empty input sends nothing.
        assert!(
            s.update(Msg::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::CONTROL
            )))
            .is_empty()
        );
    }

    #[test]
    fn send_without_session_creates_then_submits() {
        let mut s = TuiState::new("m".into(), TurnMode::Plan);
        typed(&mut s, "hi");
        let cmds = s.update(ctrl('s'));
        assert!(matches!(&cmds[..], [Cmd::CreateSession { .. }]));
        let cmds = s.update(Msg::Created(Box::new(info("new"))));
        assert!(matches!(&cmds[..],
            [Cmd::Submit { session_id, req }] if session_id == "new" && req.prompt == "hi" && req.mode == Some(TurnMode::Plan)));
        assert_eq!(s.current.as_deref(), Some("new"));
    }

    #[test]
    fn tab_toggles_mode_esc_cancels_running() {
        let mut s = state();
        s.update(key(KeyCode::Tab));
        assert_eq!(s.mode, TurnMode::Plan);
        assert!(s.update(key(KeyCode::Esc)).is_empty());
        s.update(ev(
            1,
            EventKind::TurnStart {
                turn_id: "t".into(),
            },
        ));
        assert!(matches!(&s.update(key(KeyCode::Esc))[..], [Cmd::Cancel(id)] if id == "s1"));
    }

    #[test]
    fn quit_ctrl_c_twice_or_ctrl_q() {
        let mut s = state();
        assert!(s.update(ctrl('c')).is_empty());
        assert!(s.quit_armed());
        // Any other key disarms.
        s.update(key(KeyCode::Char('a')));
        assert!(!s.quit_armed());
        s.update(ctrl('c'));
        assert!(matches!(&s.update(ctrl('c'))[..], [Cmd::Quit]));
        assert!(matches!(&state().update(ctrl('q'))[..], [Cmd::Quit]));
    }

    #[test]
    fn running_quit_cancels_first() {
        let mut s = state();
        s.update(ev(
            1,
            EventKind::TurnStart {
                turn_id: "t".into(),
            },
        ));
        assert!(matches!(
            &s.update(ctrl('q'))[..],
            [Cmd::Cancel(_), Cmd::Quit]
        ));
    }

    #[test]
    fn scroll_clamped_and_follow() {
        let mut s = state();
        s.scroll_max.set(15);
        s.update(Msg::Resize { height: 18 });
        s.update(key(KeyCode::PageUp));
        s.update(key(KeyCode::PageUp));
        assert_eq!(s.scroll, 15);
        s.update(key(KeyCode::PageDown));
        assert_eq!(s.scroll, 5);
        s.update(key(KeyCode::PageDown));
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn session_switch_loads_and_replays_backlog_without_dupes() {
        let mut s = state();
        s.sessions = vec![
            info("s1"),
            SessionInfo {
                model: Some("other".into()),
                ..info("s2")
            },
        ];
        s.update(ctrl('b'));
        assert_eq!(s.focus, Focus::Sessions);
        s.update(key(KeyCode::Down));
        let cmds = s.update(key(KeyCode::Enter));
        assert!(matches!(&cmds[..], [Cmd::Load(id)] if id == "s2"));
        assert_eq!(s.model, "other");
        // Event lands mid-load, also present in the snapshot.
        s.update(ev(
            1,
            EventKind::UserMessage {
                content: "q".into(),
            },
        ));
        s.update(ev(
            2,
            EventKind::UserMessage {
                content: "later".into(),
            },
        ));
        s.update(Msg::Loaded {
            session_id: "s2".into(),
            events: vec![SessionEvent {
                seq: 1,
                ts_ms: 0,
                kind: EventKind::UserMessage {
                    content: "q".into(),
                },
            }],
            pending: vec![],
        });
        assert_eq!(
            s.blocks,
            vec![Block::User("q".into()), Block::User("later".into())]
        );
    }

    #[test]
    fn stale_pending_approvals_dropped_on_load() {
        let mut s = state();
        s.begin_session("s1", true);
        let req = |n, id: &str| SessionEvent {
            seq: n,
            ts_ms: 0,
            kind: EventKind::ApprovalRequest {
                approval_id: id.into(),
                call_id: "c".into(),
                tool: "t".into(),
                detail: None,
            },
        };
        s.update(Msg::Loaded {
            session_id: "s1".into(),
            events: vec![req(1, "old"), req(2, "live")],
            pending: vec!["live".into()],
        });
        assert_eq!(s.approvals.len(), 1);
        assert_eq!(s.approvals[0].id, "live");
    }

    #[test]
    fn model_picker_selects() {
        use hk_llm::{Capabilities, ModelInfo};
        let m = |id: &str| ModelInfo {
            id: id.into(),
            provider: "p".into(),
            name: id.into(),
            context_window: 1000,
            max_output: None,
            capabilities: Capabilities::default(),
            cost: None,
        };
        let mut s = state();
        s.update(Msg::Models(vec![
            m("deepseek-chat"),
            m("deepseek-reasoner"),
        ]));
        s.update(ctrl('o'));
        assert!(matches!(s.overlay, Overlay::Models { sel: 0 }));
        s.update(key(KeyCode::Down));
        s.update(key(KeyCode::Enter));
        assert_eq!(s.model, "deepseek-reasoner");
        assert_eq!(s.overlay, Overlay::None);
    }

    #[test]
    fn lag_reloads_session() {
        let mut s = state();
        let cmds = s.update(Msg::Conn(Conn::Lagged));
        assert!(matches!(&cmds[..], [Cmd::Load(id)] if id == "s1"));
    }

    #[test]
    fn feed_mapping() {
        let stream = ServerEvent {
            name: "agent/assistant-stream".into(),
            session_id: "s".into(),
            data: serde_json::json!({"turn_id":"t","step":2,"event":{"type":"text_delta","text":"x"}}),
        };
        assert!(matches!(
            Msg::from_feed(&stream),
            Some(Msg::Stream { step: 2, .. })
        ));
        let log = SessionEvent {
            seq: 7,
            ts_ms: 1,
            kind: EventKind::UserMessage {
                content: "hi".into(),
            },
        };
        let ev = ServerEvent {
            name: "user/message".into(),
            session_id: "s".into(),
            data: serde_json::to_value(&log).unwrap(),
        };
        assert!(matches!(Msg::from_feed(&ev), Some(Msg::Log(e)) if e.seq == 7));
    }

    #[test]
    fn input_editing_unicode() {
        let mut i = Input::default();
        i.insert("héy\nx");
        i.vertical(false);
        assert_eq!(i.cursor, 1);
        i.end();
        i.left();
        i.backspace();
        assert_eq!(i.text, "hy\nx");
    }
}
