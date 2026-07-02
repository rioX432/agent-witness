//! `agent-witness show <session>`: a ratatui timeline viewer with a detail pane
//! and live follow.
//!
//! Design (established ratatui idioms; API pinned against ratatui 0.30 docs via
//! Context7):
//! - [`TuiApp`] holds all UI state; it is built from a [`SessionRead`] and never
//!   touches the wall-clock — every displayed time derives from event data.
//! - [`render`] is a pure function of `(&TuiApp)` into a [`Frame`], so a
//!   `TestBackend` renders a byte-for-byte deterministic screen (golden tests).
//! - [`handle_key`] is a pure state transition, unit-testable without a terminal.
//! - [`run_show`] owns the only impure parts: the crossterm event loop and
//!   polling the JSONL file for appended events (live follow, no fs-watcher —
//!   fs watching is deferred to v0.2 per CLAUDE.md).
//!
//! Honesty surface (ADR-0002): corrupted-line counts and per-row attribution
//! markers are shown in the header and rows, never hidden.

use std::time::Duration;

use agent_witness_core::{Attribution, SessionRead, SessionStore, Source};
use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use serde_json::Value;

use crate::timefmt::{format_duration_ms, format_offset_ms, format_utc};
use crate::timeline::{build_timeline, tool_call_count, TimelineEntry};

/// Header block height (2 content lines + top/bottom border).
const HEADER_HEIGHT: u16 = 4;
/// Footer height (single hint line).
const FOOTER_HEIGHT: u16 = 1;
/// When the detail pane is open, share the body between list and detail evenly.
const HALF: u16 = 50;
/// Column width for the relative-time offset (e.g. `+1234.567s`).
const OFFSET_WIDTH: usize = 10;
/// Column width for the tool/kind tag.
const TAG_WIDTH: usize = 12;
/// Column width for the tool status label (widest is `no-result`).
const STATUS_WIDTH: usize = 9;
/// How often live follow re-reads the session log.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Placeholder for an absent value.
const ABSENT: &str = "-";

/// All state the timeline viewer renders from. Pure data: no handles, no clock.
#[derive(Debug, Clone)]
pub struct TuiApp {
    /// Session being viewed.
    pub session_id: String,
    /// Time of the first event, used as the base for relative offsets.
    pub base_ts: i64,
    /// Grouped timeline rows.
    pub entries: Vec<TimelineEntry>,
    /// Index of the highlighted row.
    pub selected: usize,
    /// Whether the detail pane is open.
    pub detail_open: bool,
    /// Whether live follow (tail) is enabled.
    pub follow: bool,
    /// Total parsed events (for the header).
    pub event_count: usize,
    /// Tool-call count (for the header).
    pub tool_count: usize,
    /// Corrupted/unparseable lines skipped (honesty surface).
    pub skipped_lines: usize,
    /// Set when the user asks to quit.
    pub should_quit: bool,
}

impl TuiApp {
    /// Build initial state from a session read.
    pub fn new(session_id: impl Into<String>, read: &SessionRead) -> Self {
        Self {
            session_id: session_id.into(),
            base_ts: read.events.first().map(|e| e.ts).unwrap_or(0),
            entries: build_timeline(&read.events),
            selected: 0,
            detail_open: false,
            follow: false,
            event_count: read.events.len(),
            tool_count: tool_call_count(&read.events),
            skipped_lines: read.skipped_lines,
            should_quit: false,
        }
    }

    /// Refresh from a newer read (live follow). Keeps the selection stable, but
    /// when following and already at the tail, sticks to the newest row.
    pub fn update(&mut self, read: &SessionRead) {
        let was_at_tail = self.selected + 1 >= self.entries.len().max(1);
        self.entries = build_timeline(&read.events);
        if let Some(first) = read.events.first() {
            self.base_ts = first.ts;
        }
        self.event_count = read.events.len();
        self.tool_count = tool_call_count(&read.events);
        self.skipped_lines = read.skipped_lines;
        if self.follow && was_at_tail {
            self.selected = self.entries.len().saturating_sub(1);
        } else {
            self.clamp_selection();
        }
    }

    fn clamp_selection(&mut self) {
        let last = self.entries.len().saturating_sub(1);
        if self.selected > last {
            self.selected = last;
        }
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
}

/// Apply one key press to the app state. Pure and terminal-independent.
pub fn handle_key(app: &mut TuiApp, code: KeyCode) {
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        // Esc backs out of the detail pane first, then quits.
        KeyCode::Esc => {
            if app.detail_open {
                app.detail_open = false;
            } else {
                app.should_quit = true;
            }
        }
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::Enter => {
            if !app.entries.is_empty() {
                app.detail_open = !app.detail_open;
            }
        }
        KeyCode::Char('f') => app.follow = !app.follow,
        _ => {}
    }
}

/// Render the whole screen. Pure function of `app` — the golden-test seam.
pub fn render(frame: &mut Frame, app: &TuiApp) {
    let chunks = Layout::vertical([
        Constraint::Length(HEADER_HEIGHT),
        Constraint::Min(1),
        Constraint::Length(FOOTER_HEIGHT),
    ])
    .split(frame.area());

    render_header(frame, app, chunks[0]);

    let body = chunks[1];
    if app.detail_open && !app.entries.is_empty() {
        let split = Layout::vertical([Constraint::Percentage(HALF), Constraint::Percentage(HALF)])
            .split(body);
        render_list(frame, app, split[0]);
        render_detail(frame, app, split[1]);
    } else {
        render_list(frame, app, body);
    }

    render_footer(frame, chunks[2]);
}

fn render_header(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let started = if app.event_count == 0 {
        ABSENT.to_string()
    } else {
        format_utc(app.base_ts)
    };
    let follow = if app.follow { "on" } else { "off" };
    let lines = vec![
        Line::from(format!("session {}  ·  started {started}", app.session_id)),
        Line::from(format!(
            "{} events · {} tool calls · {} corrupt lines · follow {follow}",
            app.event_count, app.tool_count, app.skipped_lines
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("agent-witness")),
        area,
    );
}

fn render_list(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let block = Block::bordered().title("timeline");
    if app.entries.is_empty() {
        frame.render_widget(
            Paragraph::new("(no events recorded yet)").block(block),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let selected = i == app.selected;
            let marker = if selected { ">" } else { " " };
            let offset = format_offset_ms(entry.ts - app.base_ts);
            let attr = attribution_char(entry.attribution);
            let status = entry.tool_status.map(|s| s.label()).unwrap_or("");
            // Fixed-width columns keep rows aligned; ratatui clips the trailing
            // summary to the pane, so an over-long line never breaks the layout.
            let text = format!(
                "{marker} {offset:>ow$} {attr} {tag:<tw$} {status:<sw$} {summary}",
                tag = entry.tag,
                summary = entry.summary,
                ow = OFFSET_WIDTH,
                tw = TAG_WIDTH,
                sw = STATUS_WIDTH,
            );
            let style = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(text)).style(style)
        })
        .collect();

    frame.render_widget(List::new(items).block(block), area);
}

fn render_detail(frame: &mut Frame, app: &TuiApp, area: Rect) {
    let text = app
        .entries
        .get(app.selected)
        .map(detail_text)
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::bordered().title("detail"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("j/k move · Enter detail · f follow · q quit · Esc back")
            .style(Style::default().add_modifier(Modifier::DIM)),
        area,
    );
}

/// Build the detail-pane text for one entry: provenance first (honesty), then
/// the tool input/response (or the full payload for non-tool events).
fn detail_text(entry: &TimelineEntry) -> String {
    let mut out = String::new();
    let kind_word = if entry.tool_status.is_some() {
        "tool"
    } else {
        "event"
    };
    out.push_str(&format!("{kind_word}: {}\n", entry.tag));
    out.push_str(&format!(
        "attribution: {} · source: {} · confidence: {}\n",
        attribution_word(entry.attribution),
        source_word(entry.source),
        entry.call.confidence,
    ));
    if let Some(raw_ref) = entry.call.raw_event_ref.as_deref() {
        out.push_str(&format!("raw_ref: {raw_ref}\n"));
    }
    if let Some(status) = entry.tool_status {
        out.push_str(&format!("status: {}", status.label()));
        if let Some(duration) = entry.duration_ms {
            out.push_str(&format!(" · duration: {}", format_duration_ms(duration)));
        }
        out.push('\n');
    }
    out.push('\n');

    if entry.tool_status.is_some() {
        if let Some(input) = entry.call.payload.get("tool_input") {
            out.push_str("input:\n");
            out.push_str(&pretty(input));
            out.push('\n');
        }
        if let Some(response) = entry
            .result
            .as_ref()
            .and_then(|r| r.payload.get("tool_response"))
        {
            out.push_str("\nresponse:\n");
            out.push_str(&pretty(response));
            out.push('\n');
        }
    } else {
        out.push_str("payload:\n");
        out.push_str(&pretty(&entry.call.payload));
        out.push('\n');
    }
    out
}

/// Pretty-print JSON, falling back to compact form if pretty-printing fails.
fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Single-letter attribution marker for a timeline row.
fn attribution_char(attribution: Attribution) -> &'static str {
    match attribution {
        Attribution::Direct => "D",
        Attribution::Observed => "O",
        Attribution::Inferred => "I",
    }
}

/// Full attribution word for the detail pane.
fn attribution_word(attribution: Attribution) -> &'static str {
    match attribution {
        Attribution::Direct => "direct",
        Attribution::Observed => "observed",
        Attribution::Inferred => "inferred",
    }
}

/// Source word for the detail pane.
fn source_word(source: Source) -> &'static str {
    match source {
        Source::Hooks => "hooks",
        Source::Transcript => "transcript",
    }
}

/// Re-read the session and refresh the app if the log grew or gained corrupt
/// lines (one live-follow step). Returns whether the app was updated. Shared by
/// the run loop and the follow integration test so both exercise the same
/// grow-and-refresh logic (issue #19).
pub fn poll_update(app: &mut TuiApp, store: &SessionStore, session_id: &str) -> Result<bool> {
    let fresh = store.read(session_id)?;
    // Only rebuild when the log actually grew or gained corrupt lines, to avoid
    // needless churn while idle.
    if fresh.events.len() != app.event_count || fresh.skipped_lines != app.skipped_lines {
        app.update(&fresh);
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Run the interactive viewer against a stored session. Owns the terminal
/// lifecycle and the live-follow polling loop. When `follow` is set the viewer
/// starts in tail mode (unifies `show --follow` with the in-TUI `f` toggle —
/// issue #19).
pub fn run_show(store: &SessionStore, session_id: &str, follow: bool) -> Result<()> {
    let read = store.read(session_id)?;
    let mut app = TuiApp::new(session_id, &read);
    app.follow = follow;

    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &mut app, store, session_id);
    ratatui::restore();
    result
}

fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut TuiApp,
    store: &SessionStore,
    session_id: &str,
) -> Result<()> {
    loop {
        terminal.draw(|frame| render(frame, app))?;

        if event::poll(POLL_INTERVAL)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(app, key.code);
                }
            }
        }

        if app.should_quit {
            break;
        }

        if app.follow {
            poll_update(app, store, session_id)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_witness_core::{AgentEvent, EventKind, CONFIDENCE_CERTAIN};
    use serde_json::json;

    fn read_with(events: Vec<AgentEvent>, skipped: usize) -> SessionRead {
        SessionRead {
            events,
            skipped_lines: skipped,
        }
    }

    fn tool_call(ts: i64, id: &str) -> AgentEvent {
        AgentEvent::new(
            ts,
            "s",
            Source::Hooks,
            EventKind::ToolCall,
            Attribution::Direct,
            CONFIDENCE_CERTAIN,
            json!({"tool_name": "Bash", "tool_use_id": id, "tool_input": {"command": "ls"}}),
        )
    }

    #[test]
    fn new_app_starts_at_top_with_counts() {
        let app = TuiApp::new(
            "s",
            &read_with(vec![tool_call(1, "a"), tool_call(2, "b")], 3),
        );
        assert_eq!(app.selected, 0);
        assert_eq!(app.event_count, 2);
        assert_eq!(app.tool_count, 2);
        assert_eq!(app.skipped_lines, 3);
        assert!(!app.detail_open);
        assert!(!app.follow);
    }

    #[test]
    fn j_k_move_and_clamp_at_bounds() {
        let mut app = TuiApp::new(
            "s",
            &read_with(vec![tool_call(1, "a"), tool_call(2, "b")], 0),
        );
        handle_key(&mut app, KeyCode::Char('k')); // already at top
        assert_eq!(app.selected, 0);
        handle_key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected, 1);
        handle_key(&mut app, KeyCode::Char('j')); // clamp at bottom
        assert_eq!(app.selected, 1);
        handle_key(&mut app, KeyCode::Up);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn enter_toggles_detail_and_esc_backs_out_then_quits() {
        let mut app = TuiApp::new("s", &read_with(vec![tool_call(1, "a")], 0));
        handle_key(&mut app, KeyCode::Enter);
        assert!(app.detail_open);
        handle_key(&mut app, KeyCode::Esc); // closes detail, not quit
        assert!(!app.detail_open);
        assert!(!app.should_quit);
        handle_key(&mut app, KeyCode::Esc); // now quits
        assert!(app.should_quit);
    }

    #[test]
    fn enter_is_ignored_on_empty_session() {
        let mut app = TuiApp::new("s", &read_with(vec![], 0));
        handle_key(&mut app, KeyCode::Enter);
        assert!(!app.detail_open);
    }

    #[test]
    fn f_toggles_follow_and_q_quits() {
        let mut app = TuiApp::new("s", &read_with(vec![tool_call(1, "a")], 0));
        handle_key(&mut app, KeyCode::Char('f'));
        assert!(app.follow);
        handle_key(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn update_in_follow_sticks_to_tail() {
        let mut app = TuiApp::new("s", &read_with(vec![tool_call(1, "a")], 0));
        app.follow = true;
        app.update(&read_with(
            vec![tool_call(1, "a"), tool_call(2, "b"), tool_call(3, "c")],
            0,
        ));
        assert_eq!(app.selected, 2);
        assert_eq!(app.event_count, 3);
    }

    #[test]
    fn update_without_follow_keeps_selection_but_clamps() {
        let mut app = TuiApp::new(
            "s",
            &read_with(
                vec![tool_call(1, "a"), tool_call(2, "b"), tool_call(3, "c")],
                0,
            ),
        );
        app.selected = 2;
        // Log shrank (shouldn't normally happen, but must not panic): clamp.
        app.update(&read_with(vec![tool_call(1, "a")], 0));
        assert_eq!(app.selected, 0);
    }
}
