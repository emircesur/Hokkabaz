//! Rendering: `&TuiState` -> ratatui widgets. No mutation beyond the `scroll_max` cell.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Frame_, Clear, List, ListItem, ListState, Paragraph};
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

use super::state::{Block, Conn, Focus, Overlay, ToolCard, TuiState};
use hk_server::TurnMode;

const SIDEBAR_W: u16 = 30;
const COLLAPSED_RESULT_LINES: usize = 4;
const COLLAPSED_WRITE_LINES: usize = 12;

const HELP: &[(&str, &str)] = &[
    (
        "Alt+Enter / Ctrl+Enter / Ctrl+S",
        "send message (Enter = newline)",
    ),
    ("Esc", "cancel running turn / leave sessions pane"),
    ("Tab", "toggle mode build <-> plan"),
    ("Ctrl+O / Ctrl+M", "switch model"),
    ("Ctrl+B", "focus sessions pane (j/k, Enter open, n new)"),
    ("Ctrl+N", "new session"),
    ("Ctrl+R", "expand / collapse reasoning"),
    ("Ctrl+E", "expand / collapse tool output"),
    (
        "PgUp / PgDn",
        "scroll transcript (End on empty input: follow)",
    ),
    ("? / F1", "this help (? on empty input)"),
    ("Ctrl+C x2 / Ctrl+Q", "quit"),
    (
        "approval prompt",
        "y approve, n or Esc deny, r deny with reason",
    ),
];

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn bold(c: Color) -> Style {
    Style::new().fg(c).add_modifier(Modifier::BOLD)
}

/// Model/tool text can carry escape sequences; never let them reach the terminal.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .map(|c| if c == '\t' { ' ' } else { c })
        .collect()
}

/// Hard-wrap one logical line to `width` display columns (at least one char per row).
fn wrap(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut cur = String::new();
    let mut w = 0;
    for c in line.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width && !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
            w = 0;
        }
        cur.push(c);
        w += cw;
    }
    rows.push(cur);
    rows
}

/// Push `text` as wrapped rows; the first row gets `first`, the rest `rest`.
fn push_text(
    out: &mut Vec<Line<'static>>,
    first: &str,
    rest: &str,
    style: Style,
    text: &str,
    width: usize,
) {
    let indent = first.chars().count().max(rest.chars().count());
    let mut head = true;
    for logical in sanitize(text).split('\n') {
        for row in wrap(logical, width.saturating_sub(indent)) {
            let pre = if head { first } else { rest };
            head = false;
            out.push(Line::from(vec![
                Span::styled(format!("{pre:<indent$}"), style),
                Span::styled(row, style),
            ]));
        }
    }
}

fn diff_style(line: &str) -> Style {
    match line.as_bytes().first() {
        Some(b'+') if !line.starts_with("+++") => Style::new().fg(Color::Green),
        Some(b'-') if !line.starts_with("---") => Style::new().fg(Color::Red),
        Some(b'@') => Style::new().fg(Color::Cyan),
        _ => dim(),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn tool_summary(c: &ToolCard) -> String {
    [
        "command",
        "path",
        "pattern",
        "url",
        "file_path",
        "description",
    ]
    .iter()
    .find_map(|k| arg_str(&c.args, k))
    .map(|s| s.lines().next().unwrap_or("").to_owned())
    .unwrap_or_else(|| c.args.to_string())
}

fn tool_card(out: &mut Vec<Line<'static>>, c: &ToolCard, expand: bool, width: usize) {
    let (mark, color) = match &c.result {
        None => ("...", Color::Yellow),
        Some((_, false)) => ("ok", Color::Green),
        Some((_, true)) => ("err", Color::Red),
    };
    let head = format!("{} {}", c.name, tool_summary(c));
    let mut row = Vec::new();
    push_text(
        &mut row,
        "",
        "",
        bold(Color::Magenta),
        &head,
        width.saturating_sub(mark.len() + 4),
    );
    if let Some(first) = row.first_mut() {
        first
            .spans
            .insert(0, Span::styled(format!("[{mark}] "), bold(color)));
    }
    for (i, mut l) in row.into_iter().enumerate() {
        if i > 0 {
            l.spans.insert(0, Span::raw("     "));
        }
        out.push(l);
    }

    let body_w = width.saturating_sub(2);
    let push_diff =
        |out: &mut Vec<Line<'static>>, sign: &str, text: &str, style: Style, cap: usize| {
            let lines: Vec<&str> = text.lines().collect();
            let shown = if expand {
                lines.len()
            } else {
                lines.len().min(cap)
            };
            for l in &lines[..shown] {
                push_text(out, &format!("  {sign} "), "    ", style, l, body_w + 2);
            }
            if shown < lines.len() {
                out.push(Line::styled(
                    format!("    ... {} more lines (Ctrl+E)", lines.len() - shown),
                    dim(),
                ));
            }
        };
    match c.name.as_str() {
        "edit" => {
            push_diff(
                out,
                "-",
                arg_str(&c.args, "old_string").unwrap_or(""),
                diff_style("-"),
                COLLAPSED_WRITE_LINES,
            );
            push_diff(
                out,
                "+",
                arg_str(&c.args, "new_string").unwrap_or(""),
                diff_style("+"),
                COLLAPSED_WRITE_LINES,
            );
        }
        "write" => {
            push_diff(
                out,
                "+",
                arg_str(&c.args, "content").unwrap_or(""),
                diff_style("+"),
                COLLAPSED_WRITE_LINES,
            );
        }
        _ if expand => {
            let pretty = serde_json::to_string_pretty(&c.args).unwrap_or_default();
            for l in pretty.lines() {
                push_text(out, "  args ", "       ", dim(), l, width);
            }
        }
        _ => {}
    }

    if let Some((content, is_error)) = &c.result {
        let lines: Vec<&str> = content.lines().collect();
        let shown = if expand {
            lines.len()
        } else {
            lines.len().min(COLLAPSED_RESULT_LINES)
        };
        let is_diff = lines.iter().any(|l| l.starts_with("@@"));
        for l in &lines[..shown] {
            let style = if *is_error {
                Style::new().fg(Color::Red)
            } else if is_diff {
                diff_style(l)
            } else {
                dim()
            };
            push_text(out, "  | ", "  | ", style, l, width);
        }
        if shown < lines.len() {
            out.push(Line::styled(
                format!("  | ... {} more lines (Ctrl+E)", lines.len() - shown),
                dim(),
            ));
        }
    }
}

fn assistant(
    out: &mut Vec<Line<'static>>,
    text: &str,
    reasoning: &str,
    st: &TuiState,
    width: usize,
    live: bool,
) {
    if !reasoning.is_empty() {
        if st.show_reasoning {
            out.push(Line::styled(
                "~ reasoning",
                dim().add_modifier(Modifier::ITALIC),
            ));
            push_text(
                out,
                "  ",
                "  ",
                dim().add_modifier(Modifier::ITALIC),
                reasoning,
                width,
            );
        } else {
            let n = reasoning.chars().count();
            out.push(Line::styled(
                format!("~ reasoning ({n} chars, Ctrl+R to expand)"),
                dim(),
            ));
        }
    }
    if !text.is_empty() {
        push_text(out, "", "", Style::new(), text, width);
    }
    if live {
        out.push(Line::styled("...", dim()));
    }
}

/// All transcript rows, wrapped to `width`.
pub fn transcript(st: &TuiState, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for b in &st.blocks {
        match b {
            Block::User(t) => {
                if !out.is_empty() {
                    out.push(Line::raw(""));
                }
                push_text(&mut out, "> ", "  ", bold(Color::Cyan), t, width);
            }
            Block::Assistant { text, reasoning } => {
                assistant(&mut out, text, reasoning, st, width, false)
            }
            Block::Tool(c) => tool_card(&mut out, c, st.expand_tools, width),
            Block::Note { text, error } => {
                let style = if *error {
                    Style::new().fg(Color::Red)
                } else {
                    dim()
                };
                push_text(&mut out, "-- ", "   ", style, text, width);
            }
        }
    }
    if let Some(l) = &st.live {
        assistant(&mut out, &l.text, &l.reasoning, st, width, true);
    }
    out
}

fn fmt_tokens(n: u64) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{}.{}k", n / 1000, n % 1000 / 100)
    } else {
        n.to_string()
    }
}

fn status_line(st: &TuiState) -> Line<'static> {
    let mode = match st.mode {
        TurnMode::Build => Span::styled(" BUILD ", Style::new().fg(Color::Black).bg(Color::Green)),
        TurnMode::Plan => Span::styled(" PLAN ", Style::new().fg(Color::Black).bg(Color::Yellow)),
    };
    let (conn, c) = match st.conn {
        Conn::Live => ("live", Color::Green),
        Conn::Lagged => ("resync", Color::Yellow),
        Conn::Closed => ("disconnected", Color::Red),
    };
    let ctx = st
        .models
        .iter()
        .find(|m| m.id == st.model)
        .map(|m| format!("/{}", fmt_tokens(m.context_window)))
        .unwrap_or_default();
    let mut spans = vec![
        mode,
        Span::raw(format!(
            " {} ",
            if st.model.is_empty() { "-" } else { &st.model }
        )),
        Span::styled(
            format!(
                "| in {} out {}{} ",
                fmt_tokens(st.usage.total_input()),
                fmt_tokens(st.usage.output_tokens),
                ctx
            ),
            dim(),
        ),
        Span::styled(format!("| {conn} "), Style::new().fg(c)),
    ];
    if st.running {
        spans.push(Span::styled(
            "| running (Esc cancels) ",
            Style::new().fg(Color::Yellow),
        ));
    }
    if let Some(f) = &st.flash {
        spans.push(Span::styled(
            format!("| {f} "),
            Style::new().fg(Color::Cyan),
        ));
    } else if st.quit_armed() {
        spans.push(Span::styled(
            "| Ctrl+C again to quit ",
            Style::new().fg(Color::Red),
        ));
    } else {
        spans.push(Span::styled("| ? help ", dim()));
    }
    Line::from(spans)
}

/// Hard-wrapped input rows plus the (row, col) of the cursor.
pub fn input_layout(text: &str, cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let (mut r, mut w) = (0usize, 0usize);
    let mut pos = (0, 0);
    for (i, c) in text.char_indices() {
        if c == '\n' {
            if i == cursor {
                pos = (r, w);
            }
            rows.push(String::new());
            r += 1;
            w = 0;
            continue;
        }
        let cw = c.width().unwrap_or(0);
        if w + cw > width {
            rows.push(String::new());
            r += 1;
            w = 0;
        }
        if i == cursor {
            pos = (r, w);
        }
        rows[r].push(c);
        w += cw;
    }
    if cursor >= text.len() {
        pos = (r, w);
    }
    (rows, pos)
}

pub fn draw(f: &mut Frame, st: &TuiState) {
    let area = f.area();
    let in_lines = st.input.text.split('\n').count().clamp(1, 6) as u16;
    let [body, input_a, status_a] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(in_lines + 2),
        Constraint::Length(1),
    ])
    .areas(area);

    let show_side = area.width >= 80 || st.focus == Focus::Sessions;
    let (side, main) = if show_side {
        let [s, m] = Layout::horizontal([
            Constraint::Length(SIDEBAR_W.min(area.width / 2)),
            Constraint::Min(10),
        ])
        .areas(body);
        (Some(s), m)
    } else {
        (None, body)
    };
    if let Some(s) = side {
        draw_sessions(f, st, s);
    }
    draw_transcript(f, st, main);
    draw_input(f, st, input_a);
    f.render_widget(Paragraph::new(status_line(st)), status_a);

    if let Some(a) = st.approvals.front() {
        draw_approval(f, st, a, area);
    } else {
        match &st.overlay {
            Overlay::Help => draw_help(f, area),
            Overlay::Models { sel } => draw_models(f, st, *sel, area),
            Overlay::None => {}
        }
    }
}

fn pane(title: &str, focused: bool) -> Frame_<'_> {
    let c = if focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };
    Frame_::bordered()
        .title(title)
        .border_style(Style::new().fg(c))
}

fn draw_sessions(f: &mut Frame, st: &TuiState, area: Rect) {
    let w = usize::from(area.width.saturating_sub(4));
    let items: Vec<ListItem> = st
        .sessions
        .iter()
        .map(|s| {
            let name = s
                .title
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| s.id.chars().take(12).collect());
            let name = sanitize(&name).replace('\n', " ");
            let mark = if s.running { "* " } else { "  " };
            let cur = Some(&s.id) == st.current.as_ref();
            let text: String = format!("{mark}{name}").chars().take(w).collect();
            ListItem::new(text).style(if cur {
                bold(Color::White)
            } else {
                Style::new()
            })
        })
        .collect();
    let list = List::new(items)
        .block(pane("Sessions", st.focus == Focus::Sessions))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut ls = ListState::default();
    if st.focus == Focus::Sessions {
        ls.select(Some(st.sel));
    }
    f.render_stateful_widget(list, area, &mut ls);
}

fn draw_transcript(f: &mut Frame, st: &TuiState, area: Rect) {
    let title = match &st.current {
        Some(id) => format!("Session {}", id.chars().take(12).collect::<String>()),
        None => "No session (type a message to start)".to_owned(),
    };
    let block = pane(&title, false);
    let inner = block.inner(area);
    let rows = transcript(st, usize::from(inner.width));
    let h = usize::from(inner.height);
    let max = rows.len().saturating_sub(h);
    st.scroll_max.set(max);
    let scroll = st.scroll.min(max);
    let end = rows.len() - scroll;
    let start = end.saturating_sub(h);
    f.render_widget(Paragraph::new(rows[start..end].to_vec()).block(block), area);
    if scroll > 0 {
        let tag = format!(" {scroll} lines up ");
        let r = Rect::new(
            area.right().saturating_sub(tag.len() as u16 + 2),
            area.bottom().saturating_sub(1),
            tag.len() as u16,
            1,
        );
        f.render_widget(
            Paragraph::new(Span::styled(
                tag,
                Style::new().fg(Color::Black).bg(Color::Yellow),
            )),
            r,
        );
    }
}

fn draw_input(f: &mut Frame, st: &TuiState, area: Rect) {
    let title = if st.current.is_some() {
        "Message"
    } else {
        "Message (new session)"
    };
    let block = pane(title, st.focus == Focus::Input && st.approvals.is_empty()).title_bottom(
        Line::styled(" Alt+Enter send | Enter newline ", dim()).alignment(Alignment::Right),
    );
    let inner = block.inner(area);
    let (rows, (cr, cc)) = input_layout(&st.input.text, st.input.cursor, usize::from(inner.width));
    let h = usize::from(inner.height);
    let first = (cr + 1).saturating_sub(h);
    let lines: Vec<Line> = rows
        .iter()
        .skip(first)
        .take(h)
        .map(|r| Line::raw(sanitize(r)))
        .collect();
    f.render_widget(Paragraph::new(lines).block(block), area);
    if st.focus == Focus::Input && st.approvals.is_empty() && st.overlay == Overlay::None {
        f.set_cursor_position(Position::new(
            inner.x + cc as u16,
            inner.y + (cr - first) as u16,
        ));
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_help(f: &mut Frame, area: Rect) {
    let r = centered(area, 76, HELP.len() as u16 + 4);
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("{k:<34}"), bold(Color::Cyan)),
                Span::raw(*d),
            ])
        })
        .chain([Line::raw(""), Line::styled("any key closes", dim())])
        .collect();
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(Frame_::bordered().title("Help")),
        r,
    );
}

fn draw_models(f: &mut Frame, st: &TuiState, sel: usize, area: Rect) {
    let r = centered(area, 60, (st.models.len() as u16 + 2).clamp(5, 16));
    let items: Vec<ListItem> = st
        .models
        .iter()
        .map(|m| {
            let cur = if m.id == st.model { "* " } else { "  " };
            ListItem::new(format!(
                "{cur}{}  ({}, {} ctx)",
                m.id,
                m.provider,
                fmt_tokens(m.context_window)
            ))
        })
        .collect();
    let list = List::new(items)
        .block(Frame_::bordered().title("Model (Enter select, Esc close)"))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut ls = ListState::default().with_selected(Some(sel));
    f.render_widget(Clear, r);
    f.render_stateful_widget(list, r, &mut ls);
}

fn draw_approval(f: &mut Frame, st: &TuiState, a: &super::state::Approval, area: Rect) {
    let r = centered(area, 72, 12);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Approve ", bold(Color::Yellow)),
            Span::styled(sanitize(&a.tool), bold(Color::White)),
        ]),
        Line::raw(""),
    ];
    let w = usize::from(r.width.saturating_sub(4));
    if let Some(d) = &a.detail {
        push_text(&mut lines, "", "", Style::new(), d, w);
    }
    // Args of the parked call give context the one-line detail lacks (diff for edits).
    let card = st.blocks.iter().find_map(|b| match b {
        Block::Tool(c) if c.call_id == a.call_id => Some(c),
        _ => None,
    });
    if let Some(c) = card {
        if c.name == "edit" {
            for (sign, key) in [("-", "old_string"), ("+", "new_string")] {
                for l in arg_str(&c.args, key).unwrap_or("").lines().take(2) {
                    push_text(
                        &mut lines,
                        &format!("{sign} "),
                        "  ",
                        diff_style(sign),
                        l,
                        w,
                    );
                }
            }
        }
    }
    if st.approvals.len() > 1 {
        lines.push(Line::styled(
            format!("({} more waiting)", st.approvals.len() - 1),
            dim(),
        ));
    }
    lines.push(Line::raw(""));
    match &st.reason {
        Some(reason) => {
            lines.push(Line::from(vec![
                Span::styled("deny reason: ", bold(Color::Red)),
                Span::raw(sanitize(reason)),
                Span::raw("_"),
            ]));
            lines.push(Line::styled("Enter deny | Esc back", dim()));
        }
        None => lines.push(Line::from(vec![
            Span::styled("[y]", bold(Color::Green)),
            Span::raw(" approve  "),
            Span::styled("[n]", bold(Color::Red)),
            Span::raw(" deny  "),
            Span::styled("[r]", bold(Color::Yellow)),
            Span::raw(" deny with reason"),
        ])),
    }
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(
            Frame_::bordered()
                .title("Approval required")
                .border_style(Style::new().fg(Color::Yellow)),
        ),
        r,
    );
}

#[cfg(test)]
mod tests {
    use hk_server::TurnMode;
    use hk_session::{EventKind, SessionEvent};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::state::{Msg, TuiState};
    use super::*;

    fn render(st: &TuiState, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, st)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn log(st: &mut TuiState, seq: u64, kind: EventKind) {
        st.update(Msg::Log(Box::new(SessionEvent {
            seq,
            ts_ms: 0,
            kind,
        })));
    }

    #[test]
    fn renders_chat_tool_card_diff_and_status() {
        let mut st = TuiState::new("deepseek-chat".into(), TurnMode::Plan);
        st.begin_session("sess-1234", false);
        log(
            &mut st,
            1,
            EventKind::UserMessage {
                images: Vec::new(),
                content: "fix the bug".into(),
            },
        );
        log(
            &mut st,
            2,
            EventKind::AssistantMessage {
                content: "On it.".into(),
                reasoning: Some("think think".into()),
            },
        );
        log(
            &mut st,
            3,
            EventKind::ToolCall {
                call_id: "c1".into(),
                name: "edit".into(),
                arguments: serde_json::json!({"path":"src/a.rs","old_string":"let x = 1;","new_string":"let x = 2;"}),
            },
        );
        log(
            &mut st,
            4,
            EventKind::ToolResult {
                call_id: "c1".into(),
                content: "edited".into(),
                is_error: false,
            },
        );
        let out = render(&st, 100, 24);
        assert!(out.contains("> fix the bug"), "{out}");
        assert!(out.contains("~ reasoning (11 chars"), "{out}");
        assert!(out.contains("On it."), "{out}");
        assert!(out.contains("[ok] edit src/a.rs"), "{out}");
        assert!(
            out.contains("- let x = 1;") && out.contains("+ let x = 2;"),
            "{out}"
        );
        assert!(
            out.contains("PLAN") && out.contains("deepseek-chat"),
            "{out}"
        );
        assert!(out.contains("Sessions"), "{out}");
    }

    #[test]
    fn diff_lines_are_colored() {
        let mut st = TuiState::new("m".into(), TurnMode::Build);
        st.begin_session("s", false);
        log(
            &mut st,
            1,
            EventKind::ToolCall {
                call_id: "c".into(),
                name: "write".into(),
                arguments: serde_json::json!({"path":"f","content":"a\nb"}),
            },
        );
        let rows = transcript(&st, 60);
        let plus = rows.iter().find(|l| l.to_string().contains("+ a")).unwrap();
        assert_eq!(plus.spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn approval_modal_and_help_render() {
        let mut st = TuiState::new("m".into(), TurnMode::Build);
        st.begin_session("s", false);
        log(
            &mut st,
            1,
            EventKind::ApprovalRequest {
                approval_id: "a".into(),
                call_id: "c".into(),
                tool: "bash".into(),
                detail: Some("rm -rf build".into()),
            },
        );
        let out = render(&st, 100, 24);
        assert!(
            out.contains("Approval required")
                && out.contains("rm -rf build")
                && out.contains("[y] approve"),
            "{out}"
        );

        let mut st = TuiState::new("m".into(), TurnMode::Build);
        st.overlay = Overlay::Help;
        let out = render(&st, 100, 24);
        assert!(
            out.contains("switch model") && out.contains("Help"),
            "{out}"
        );
    }

    #[test]
    fn live_text_shown_and_escape_codes_stripped() {
        let mut st = TuiState::new("m".into(), TurnMode::Build);
        st.begin_session("s", false);
        st.update(Msg::Stream {
            turn_id: "t".into(),
            step: 0,
            event: hk_llm::StreamEvent::TextDelta {
                text: "hi \x1b[31mred\x1b[0m".into(),
            },
        });
        let out = render(&st, 80, 12);
        assert!(out.contains("hi [31mred[0m"), "{out}");
        assert!(!out.contains('\x1b'));
    }

    #[test]
    fn input_cursor_layout() {
        let (rows, pos) = input_layout("abcd\nef", 6, 3);
        assert_eq!(rows, vec!["abc", "d", "ef"]);
        assert_eq!(pos, (2, 1));
        assert_eq!(input_layout("", 0, 10).1, (0, 0));
    }
}
