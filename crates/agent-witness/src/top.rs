//! `agent-witness top`: an htop-like resident view of the sessions that are
//! currently live (issue #19) — the "live end" of the audit trail.
//!
//! Each live session is one row: project (from its cwd), the tool currently
//! running (an unpaired `ToolCall`), how long since its last activity, how long
//! it has been running, and its event count. Enter drills into that session's
//! timeline by reusing the [`crate::tui`] viewer; `q` quits.
//!
//! Same design seams as [`crate::tui`]/[`crate::pick`]: [`render_top`] is a pure
//! function of [`TopApp`] (golden-testable via `TestBackend`), [`handle_top_key`]
//! is a pure state transition, and the impure event/refresh loop and the
//! drilldown re-entry live in [`run_top`]. Rows are built once per refresh from
//! an injected clock — rendering never reads the wall-clock, so goldens are
//! deterministic.
//!
//! Positioning (README): this is deliberately **not** a usage/cost dashboard.
//! Token/spend aggregation is a Won't Do; `top` shows what is *happening now*,
//! honestly labelled, not what it costs.

use std::time::Duration;

use agent_witness_core::{summarize, AgentEvent, Clock, SessionRead, SessionStore, SessionSummary};
use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use crate::terminal::init as init_terminal;
use crate::timefmt::format_duration_ms;
use crate::timeline::{build_timeline, ToolStatus};
use crate::tui;

/// Header block height (1 content line + top/bottom border).
const HEADER_HEIGHT: u16 = 3;
/// Footer height (single hint line).
const FOOTER_HEIGHT: u16 = 1;
/// Refresh cadence: the resident view re-scans the store and re-reads the clock
/// on this interval. Key presses wake the loop immediately (poll returns as soon
/// as input is ready), so this bounds only the refresh rate, not input latency.
const REFRESH_INTERVAL: Duration = Duration::from_millis(1_000);
/// Placeholder for an absent value.
const ABSENT: &str = "-";
/// Column width for the (truncated) session id.
const SESSION_WIDTH: usize = 14;
/// Column width for the project name.
const PROJECT_WIDTH: usize = 18;
/// Column width for the currently-running tool.
const TOOL_WIDTH: usize = 12;
/// Column width for the "last activity" age.
const AGE_WIDTH: usize = 9;
/// Column width for the elapsed running time.
const ELAPSED_WIDTH: usize = 9;
/// Column width for the numeric event count.
const COUNT_WIDTH: usize = 7;

/// One live session, projected for display. Pure data: every time-derived field
/// is precomputed against the injected clock so rendering reads no wall-clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopRow {
    /// Session id (used for drilldown).
    pub session_id: String,
    /// Project name, derived from the session's most recent cwd.
    pub project: String,
    /// Tool currently running (an unpaired `ToolCall`), if any.
    pub running_tool: Option<String>,
    /// Time since the last observed event (`now - last_event_ts`), if known.
    pub last_activity_age_ms: Option<i64>,
    /// Time since the session started (`now - started`), if known.
    pub elapsed_ms: Option<i64>,
    /// Total parsed events.
    pub events: usize,
}

/// All state the resident view renders from. Pure data: no handles, no clock.
#[derive(Debug, Clone)]
pub struct TopApp {
    /// Live-session rows, most-recently-active first.
    pub rows: Vec<TopRow>,
    /// Index of the highlighted row.
    pub selected: usize,
    /// Set to the chosen session id when the user drills in with Enter.
    pub chosen: Option<String>,
    /// Set when the user asks to quit.
    pub should_quit: bool,
}

impl TopApp {
    /// Build a view over the given rows (already ordered for display).
    pub fn new(rows: Vec<TopRow>) -> Self {
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

/// Scan the store for live sessions and project them into rows.
///
/// I/O boundary only: reads each session, then hands off to [`top_row_from`] for
/// all display projection. `now_ms`/`window_ms` are injected so liveness and the
/// elapsed/age fields stay a pure function of recorded data.
pub fn collect_top_rows(store: &SessionStore, now_ms: i64, window_ms: i64) -> Result<Vec<TopRow>> {
    let mut live: Vec<(SessionSummary, SessionRead)> = Vec::new();
    for id in store.list_sessions()? {
        let read = store.read(&id)?;
        let created_ts = store.read_meta(&id).ok().map(|m| m.created_ts);
        let summary = summarize(&id, created_ts, &read.events);
        if summary.is_live_within(now_ms, window_ms) {
            live.push((summary, read));
        }
    }
    // Most-recently-active first, ties broken by id — the same ordering
    // convention as the pick view, so the two screens never disagree.
    live.sort_by(|(a, _), (b, _)| {
        b.recency_ms()
            .cmp(&a.recency_ms())
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(live
        .into_iter()
        .map(|(summary, read)| top_row_from(&summary, &read.events, now_ms))
        .collect())
}

/// Project one live session summary + its events into a display row. Pure.
pub fn top_row_from(summary: &SessionSummary, events: &[AgentEvent], now_ms: i64) -> TopRow {
    TopRow {
        session_id: summary.id.clone(),
        project: project_name(events),
        running_tool: running_tool(events),
        last_activity_age_ms: summary.last_event_ts.map(|ts| now_ms - ts),
        elapsed_ms: summary.started_ms().map(|ts| now_ms - ts),
        events: summary.event_count,
    }
}

/// The tool of the most recent unpaired `ToolCall` — what is running right now.
fn running_tool(events: &[AgentEvent]) -> Option<String> {
    build_timeline(events)
        .into_iter()
        .rev()
        .find(|e| e.tool_status == Some(ToolStatus::NoResult))
        .map(|e| e.tag)
}

/// Payload field carrying the working directory of a hook event.
const FIELD_CWD: &str = "cwd";

/// Project name = the basename of the cwd on the most recent event that carries
/// one (not the last *first-seen* cwd — a session that revisits an earlier
/// directory must be labelled with where it is now).
fn project_name(events: &[AgentEvent]) -> String {
    events
        .iter()
        .rev()
        .find_map(|e| e.payload.get(FIELD_CWD).and_then(|v| v.as_str()))
        .map(basename)
        .unwrap_or_else(|| ABSENT.to_string())
}

/// The last path component of `path`, or the path itself if it has none.
fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string())
}

/// Apply one key press to the view. Pure and terminal-independent.
pub fn handle_top_key(app: &mut TopApp, code: KeyCode) {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::Enter => app.confirm(),
        _ => {}
    }
}

/// Render the whole screen. Pure function of `app` — the golden-test seam.
pub fn render_top(frame: &mut Frame, app: &TopApp) {
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

fn render_header(frame: &mut Frame, app: &TopApp, area: Rect) {
    let line = Line::from(format!("{} live session(s)", app.rows.len()));
    frame.render_widget(
        Paragraph::new(line).block(Block::bordered().title("agent-witness · top")),
        area,
    );
}

fn render_list(frame: &mut Frame, app: &TopApp, area: Rect) {
    let block = Block::bordered().title("live sessions");
    if app.rows.is_empty() {
        frame.render_widget(
            Paragraph::new("(no live sessions right now)").block(block),
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
            let session = truncate(&row.session_id, SESSION_WIDTH);
            let project = truncate(&row.project, PROJECT_WIDTH);
            let tool = row
                .running_tool
                .as_deref()
                .map(|t| truncate(t, TOOL_WIDTH))
                .unwrap_or_else(|| ABSENT.to_string());
            let age = row
                .last_activity_age_ms
                .map(format_duration_ms)
                .unwrap_or_else(|| ABSENT.to_string());
            let elapsed = row
                .elapsed_ms
                .map(format_duration_ms)
                .unwrap_or_else(|| ABSENT.to_string());
            let text = format!(
                "{marker} {session:<sw$} {project:<pw$} {tool:<tw$} {age:>aw$} {elapsed:>ew$} {events:>cw$}",
                events = row.events,
                sw = SESSION_WIDTH,
                pw = PROJECT_WIDTH,
                tw = TOOL_WIDTH,
                aw = AGE_WIDTH,
                ew = ELAPSED_WIDTH,
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

/// Clip `s` to `width` chars, marking truncation with a trailing `…`.
fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// What the resident screen returned: quit, or a session to drill into.
enum TopOutcome {
    /// The user asked to quit.
    Quit,
    /// The user chose a session to open in the timeline viewer.
    Drill(String),
}

/// Run the resident `top` view. Owns the terminal lifecycle and the drilldown
/// re-entry: pressing Enter opens the chosen session's timeline (with live
/// follow), and returning from it drops back to `top`. `q` quits.
///
/// Each screen owns its own terminal init/restore so `top` and the drilled
/// `show` viewer never nest raw-mode/alternate-screen state.
pub fn run_top(store: &SessionStore, clock: &dyn Clock, window_ms: i64) -> Result<()> {
    loop {
        match run_top_screen(store, clock, window_ms)? {
            TopOutcome::Quit => return Ok(()),
            // Drill into the live session, then return to a fresh top screen.
            TopOutcome::Drill(id) => tui::run_show(store, &id, true)?,
        }
    }
}

/// Run one resident `top` screen until the user drills in or quits.
fn run_top_screen(store: &SessionStore, clock: &dyn Clock, window_ms: i64) -> Result<TopOutcome> {
    let mut terminal = init_terminal("top", "agent-witness ls --live")?;
    let result = run_top_loop(&mut terminal, store, clock, window_ms);
    ratatui::restore();
    result
}

fn run_top_loop(
    terminal: &mut DefaultTerminal,
    store: &SessionStore,
    clock: &dyn Clock,
    window_ms: i64,
) -> Result<TopOutcome> {
    // Preserve the highlighted row across refreshes even as rows come and go.
    let mut selected = 0usize;
    loop {
        let rows = collect_top_rows(store, clock.now_ms(), window_ms)?;
        let mut app = TopApp::new(rows);
        app.selected = selected.min(app.rows.len().saturating_sub(1));

        terminal.draw(|frame| render_top(frame, &app))?;

        // Drain input until the refresh tick, so spurious wakes (resize, mouse,
        // key release) redraw at most — they never trigger a full store
        // re-scan. Only the tick (or a state-changing key) reaches the outer
        // loop's collect_top_rows.
        let tick = std::time::Instant::now();
        while let Some(remaining) = REFRESH_INTERVAL.checked_sub(tick.elapsed()) {
            if !event::poll(remaining)? {
                break; // Tick elapsed: refresh with a fresh clock/store scan.
            }
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_top_key(&mut app, key.code);
                    selected = app.selected;
                    if let Some(id) = app.chosen.take() {
                        return Ok(TopOutcome::Drill(id));
                    }
                    if app.should_quit {
                        return Ok(TopOutcome::Quit);
                    }
                    // Navigation only moved the highlight: redraw, no re-scan.
                    terminal.draw(|frame| render_top(frame, &app))?;
                }
                // A resize needs a redraw but not a store re-scan.
                Event::Resize(_, _) => {
                    terminal.draw(|frame| render_top(frame, &app))?;
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_witness_core::{
        Attribution, EventKind, SessionStore, Source, CONFIDENCE_CERTAIN, DEFAULT_LIVE_WINDOW_MS,
    };
    use serde_json::json;
    use tempfile::TempDir;

    const NOW: i64 = 1_700_000_100_000;
    const CREATED: i64 = 1_700_000_000_000;

    fn ev(ts: i64, kind: EventKind, payload: serde_json::Value) -> AgentEvent {
        AgentEvent::new(
            ts,
            "s",
            Source::Hooks,
            kind,
            Attribution::Direct,
            CONFIDENCE_CERTAIN,
            payload,
        )
    }

    fn row(id: &str) -> TopRow {
        TopRow {
            session_id: id.to_string(),
            project: "proj".to_string(),
            running_tool: None,
            last_activity_age_ms: Some(1_000),
            elapsed_ms: Some(60_000),
            events: 3,
        }
    }

    #[test]
    fn navigation_clamps_at_both_ends() {
        let mut app = TopApp::new(vec![row("a"), row("b")]);
        handle_top_key(&mut app, KeyCode::Char('k')); // already at top
        assert_eq!(app.selected, 0);
        handle_top_key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected, 1);
        handle_top_key(&mut app, KeyCode::Char('j')); // clamp at bottom
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn enter_chooses_the_selected_session_for_drilldown() {
        let mut app = TopApp::new(vec![row("a"), row("b")]);
        handle_top_key(&mut app, KeyCode::Down);
        handle_top_key(&mut app, KeyCode::Enter);
        assert_eq!(app.chosen.as_deref(), Some("b"));
    }

    #[test]
    fn quit_leaves_no_choice() {
        let mut app = TopApp::new(vec![row("a")]);
        handle_top_key(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
        assert!(app.chosen.is_none());
    }

    #[test]
    fn enter_on_empty_list_is_a_no_op() {
        let mut app = TopApp::new(vec![]);
        handle_top_key(&mut app, KeyCode::Enter);
        assert!(app.chosen.is_none());
    }

    #[test]
    fn running_tool_is_the_last_unpaired_call() {
        let events = vec![
            ev(1, EventKind::SessionStart, json!({"cwd": "/w/proj"})),
            ev(
                2,
                EventKind::ToolCall,
                json!({"tool_name": "Read", "tool_use_id": "a"}),
            ),
            ev(
                3,
                EventKind::ToolResult,
                json!({"tool_name": "Read", "tool_use_id": "a"}),
            ),
            ev(
                4,
                EventKind::ToolCall,
                json!({"tool_name": "Bash", "tool_use_id": "b", "tool_input": {"command": "sleep 9"}}),
            ),
        ];
        let summary = summarize("s", Some(CREATED), &events);
        let r = top_row_from(&summary, &events, NOW);
        assert_eq!(r.running_tool.as_deref(), Some("Bash"));
        assert_eq!(r.project, "proj");
        assert_eq!(r.events, 4);
    }

    #[test]
    fn project_is_the_cwd_of_the_most_recent_event() {
        // The session starts in A, moves to B, and comes back to A: the project
        // label must be A (where it is now), not B (last first-seen cwd).
        let events = vec![
            ev(1, EventKind::SessionStart, json!({"cwd": "/w/a"})),
            ev(
                2,
                EventKind::ToolCall,
                json!({"tool_use_id": "x", "cwd": "/w/b"}),
            ),
            ev(
                3,
                EventKind::ToolCall,
                json!({"tool_use_id": "y", "cwd": "/w/a"}),
            ),
        ];
        let summary = summarize("s", Some(CREATED), &events);
        let r = top_row_from(&summary, &events, NOW);
        assert_eq!(r.project, "a");
    }

    #[test]
    fn collect_top_rows_returns_only_live_sessions() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        let mut w = store.open("live-sess", CREATED).unwrap();
        w.append(&ev(
            NOW - 1_000,
            EventKind::SessionStart,
            json!({"cwd": "/w/live"}),
        ))
        .unwrap();
        w.append(&ev(
            NOW - 500,
            EventKind::ToolCall,
            json!({"tool_name": "Bash", "tool_use_id": "x"}),
        ))
        .unwrap();
        drop(w);

        let mut w = store.open("stopped-sess", CREATED).unwrap();
        w.append(&ev(
            NOW - 1_000,
            EventKind::SessionStart,
            json!({"cwd": "/w/stop"}),
        ))
        .unwrap();
        w.append(&ev(NOW - 500, EventKind::Stop, json!({})))
            .unwrap();
        drop(w);

        let rows = collect_top_rows(&store, NOW, DEFAULT_LIVE_WINDOW_MS).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "live-sess");
        assert_eq!(rows[0].project, "live");
        assert_eq!(rows[0].running_tool.as_deref(), Some("Bash"));
    }
}
