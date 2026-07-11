//! `agent-witness show --pick`: a session picker that merges the `ls` → `show`
//! two-step into one command. Pick a session from a list, press Enter, and drop
//! straight into its timeline viewer.
//!
//! Same design seams as [`crate::tui`]: [`render_pick`] is a pure function of
//! [`PickApp`] (golden-testable via `TestBackend`), [`handle_pick_key`] is a pure
//! state transition, and [`run_pick`] owns the only impure part (the crossterm
//! event loop). Rows never read the wall-clock — the `live` flag is decided once,
//! by the caller, from an injected clock (provisional liveness, issue #19).

use agent_witness_core::SessionSummary;
use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::terminal::init as init_terminal;
use crate::timefmt::format_utc;

/// Header block height (1 content line + top/bottom border).
const HEADER_HEIGHT: u16 = 3;
/// Footer height (single hint line).
const FOOTER_HEIGHT: u16 = 1;
/// Poll cadence for the picker's key loop.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
/// Placeholder for an absent value.
const ABSENT: &str = "-";
/// Column width for the session id.
const ID_WIDTH: usize = 24;
/// Column width for the started timestamp.
const STARTED_WIDTH: usize = 21;
/// Column width for the numeric event/tool counts.
const COUNT_WIDTH: usize = 7;
/// Live marker shown for a session that is (provisionally) still running.
const LIVE_MARKER: &str = "live";

/// One selectable row in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickRow {
    /// Session id.
    pub session_id: String,
    /// Best-available start time.
    pub started_ms: Option<i64>,
    /// Total parsed events.
    pub events: usize,
    /// Number of tool invocations.
    pub tools: usize,
    /// Provisional liveness at build time (issue #19).
    pub live: bool,
}

/// All state the picker renders from. Pure data: no handles, no clock.
#[derive(Debug, Clone)]
pub struct PickApp {
    /// Rows, most-recent-first.
    pub rows: Vec<PickRow>,
    /// Index of the highlighted row.
    pub selected: usize,
    /// Set to the chosen session id when the user confirms with Enter.
    pub chosen: Option<String>,
    /// Set when the user asks to quit without choosing.
    pub should_quit: bool,
}

impl PickApp {
    /// Build a picker over the given rows (already ordered for display).
    pub fn new(rows: Vec<PickRow>) -> Self {
        Self {
            rows,
            selected: 0,
            chosen: None,
            should_quit: false,
        }
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.rows.len() {
            self.selected += 1;
        }
    }

    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn confirm(&mut self) {
        if let Some(row) = self.rows.get(self.selected) {
            self.chosen = Some(row.session_id.clone());
        }
    }
}

/// Order summaries most-recent-first and project them into picker rows. Pure.
pub fn rows_from_summaries(summaries: &[SessionSummary], now_ms: i64) -> Vec<PickRow> {
    let mut ordered: Vec<&SessionSummary> = summaries.iter().collect();
    ordered.sort_by(|a, b| {
        b.recency_ms()
            .cmp(&a.recency_ms())
            .then_with(|| a.id.cmp(&b.id))
    });
    ordered
        .into_iter()
        .map(|s| PickRow {
            session_id: s.id.clone(),
            started_ms: s.started_ms(),
            events: s.event_count,
            tools: s.tool_calls,
            live: s.is_live(now_ms),
        })
        .collect()
}

/// Apply one key press to the picker. Pure and terminal-independent.
pub fn handle_pick_key(app: &mut PickApp, code: KeyCode) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::Enter => app.confirm(),
        _ => {}
    }
}

/// Render the whole picker screen. Pure function of `app` — the golden seam.
pub fn render_pick(frame: &mut Frame, app: &PickApp) {
    let chunks = Layout::vertical([
        Constraint::Length(HEADER_HEIGHT),
        Constraint::Min(1),
        Constraint::Length(FOOTER_HEIGHT),
    ])
    .split(frame.area());

    render_header(frame, app, chunks[0]);
    render_list(frame, app, chunks[1]);
    render_footer(frame, chunks[2]);
}

fn render_header(frame: &mut Frame, app: &PickApp, area: Rect) {
    let line = Line::from(format!("{} session(s) recorded", app.rows.len()));
    frame.render_widget(
        Paragraph::new(line).block(Block::bordered().title("agent-witness · pick a session")),
        area,
    );
}

fn render_list(frame: &mut Frame, app: &PickApp, area: Rect) {
    let block = Block::bordered().title("sessions");
    if app.rows.is_empty() {
        frame.render_widget(
            Paragraph::new("(no sessions recorded yet)").block(block),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let selected = i == app.selected;
            let marker = if selected { ">" } else { " " };
            let started = row
                .started_ms
                .map(format_utc)
                .unwrap_or_else(|| ABSENT.to_string());
            let live = if row.live { LIVE_MARKER } else { "" };
            let text = format!(
                "{marker} {id:<idw$} {started:<sw$} {events:>cw$} {tools:>cw$}  {live}",
                id = row.session_id,
                events = row.events,
                tools = row.tools,
                idw = ID_WIDTH,
                sw = STARTED_WIDTH,
                cw = COUNT_WIDTH,
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

fn render_footer(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("j/k move · Enter open · q quit")
            .style(Style::default().add_modifier(Modifier::DIM)),
        area,
    );
}

/// Run the interactive picker and return the chosen session id, or `None` if the
/// user quit without choosing. Owns the terminal lifecycle.
pub fn run_pick(rows: Vec<PickRow>) -> Result<Option<String>> {
    let mut app = PickApp::new(rows);
    let mut terminal = init_terminal("show", "agent-witness report")?;
    let result = run_loop(&mut terminal, &mut app);
    ratatui::restore();
    result.map(|()| app.chosen)
}

fn run_loop(terminal: &mut DefaultTerminal, app: &mut PickApp) -> Result<()> {
    loop {
        terminal.draw(|frame| render_pick(frame, app))?;

        if event::poll(POLL_INTERVAL)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_pick_key(app, key.code);
                }
            }
        }

        if app.should_quit || app.chosen.is_some() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_witness_core::summarize;
    use agent_witness_core::{AgentEvent, Attribution, EventKind, Source, CONFIDENCE_CERTAIN};
    use serde_json::json;

    fn event(ts: i64, kind: EventKind) -> AgentEvent {
        AgentEvent::new(
            ts,
            "s",
            Source::Hooks,
            kind,
            Attribution::Direct,
            CONFIDENCE_CERTAIN,
            json!({ "tool_name": "Read" }),
        )
    }

    fn row(id: &str) -> PickRow {
        PickRow {
            session_id: id.to_string(),
            started_ms: Some(0),
            events: 1,
            tools: 0,
            live: false,
        }
    }

    #[test]
    fn rows_are_ordered_most_recent_first() {
        let old = summarize("old", Some(100), &[event(100, EventKind::Stop)]);
        let new = summarize("new", Some(500), &[event(500, EventKind::ToolCall)]);
        let rows = rows_from_summaries(&[old, new], 1_000);
        assert_eq!(rows[0].session_id, "new");
        assert_eq!(rows[1].session_id, "old");
    }

    #[test]
    fn navigation_clamps_at_both_ends() {
        let mut app = PickApp::new(vec![row("a"), row("b")]);
        handle_pick_key(&mut app, KeyCode::Char('k')); // already at top
        assert_eq!(app.selected, 0);
        handle_pick_key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected, 1);
        handle_pick_key(&mut app, KeyCode::Char('j')); // clamp at bottom
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn enter_chooses_the_selected_session() {
        let mut app = PickApp::new(vec![row("a"), row("b")]);
        handle_pick_key(&mut app, KeyCode::Down);
        handle_pick_key(&mut app, KeyCode::Enter);
        assert_eq!(app.chosen.as_deref(), Some("b"));
    }

    #[test]
    fn quit_leaves_no_choice() {
        let mut app = PickApp::new(vec![row("a")]);
        handle_pick_key(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
        assert!(app.chosen.is_none());
    }

    #[test]
    fn enter_on_empty_list_is_a_no_op() {
        let mut app = PickApp::new(vec![]);
        handle_pick_key(&mut app, KeyCode::Enter);
        assert!(app.chosen.is_none());
    }
}
