//! `agent-witness ls`: a plain-stdout listing of recorded sessions.
//!
//! Non-TUI on purpose (the issue's 実装方針): a simple aligned table is the
//! right shape for piping and for quick scanning. The `CORRUPT` column is the
//! honesty surface here — corrupted/unparseable lines are counted and shown,
//! never hidden (ADR-0002).

use agent_witness_core::{summarize, SessionStore};
use anyhow::Result;

use crate::timefmt::{format_duration_ms, format_utc};

/// A single session's summary row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LsRow {
    /// Session id (directory name).
    pub session_id: String,
    /// Whether the session is (inferred) still running — see `agent_witness_core::liveness`.
    pub live: bool,
    /// Start time (session `created_ts`, or the first event's time as a fallback).
    pub started_ms: Option<i64>,
    /// Total parsed events.
    pub events: usize,
    /// Number of tool invocations.
    pub tools: usize,
    /// Wall span from first to last event, if at least one event exists.
    pub duration_ms: Option<i64>,
    /// Corrupted/unparseable lines skipped while reading (honesty surface).
    pub corrupt: usize,
}

/// Column headers, in display order.
const HEADERS: [&str; 7] = [
    "SESSION", "STATE", "STARTED", "EVENTS", "TOOLS", "DURATION", "CORRUPT",
];
/// Number of table columns.
const COLS: usize = 7;
/// Gap between columns.
const COL_GAP: &str = "  ";
/// Placeholder for an absent value.
const ABSENT: &str = "-";
/// State cell for a live session.
const STATE_LIVE: &str = "live";
/// State cell for an idle (started-and-idle, stopped, or stale) session.
const STATE_IDLE: &str = "idle";

/// Gather a summary row for every recorded session, most-recent-first.
///
/// `now_ms` and `window_ms` are injected so the `live` verdict stays a pure
/// function of recorded data and the recency window (determinism; issue #19).
/// Rows are ordered by start time descending so the freshest session is on top;
/// a stable id tie-break keeps the order deterministic (issue #72).
pub fn collect_rows(store: &SessionStore, now_ms: i64, window_ms: i64) -> Result<Vec<LsRow>> {
    let mut rows = Vec::new();
    for id in store.list_sessions()? {
        let read = store.read(&id)?;
        let created_ts = store.read_meta(&id).ok().map(|m| m.created_ts);
        // One canonical judgement path: the same summary top/pick/selector use,
        // so `ls` can never disagree with them on what "live" means.
        let summary = summarize(&id, created_ts, &read.events);
        let duration_ms = match (summary.first_event_ts, summary.last_event_ts) {
            (Some(first), Some(last)) => Some(last - first),
            _ => None,
        };
        rows.push(LsRow {
            session_id: id,
            live: summary.is_live_within(now_ms, window_ms),
            started_ms: summary.started_ms(),
            events: summary.event_count,
            tools: summary.tool_calls,
            duration_ms,
            corrupt: read.skipped_lines,
        });
    }
    // Most-recent-first (by start), so the freshest session tops the list; a
    // stable id tie-break keeps it deterministic. Sessions with no known start
    // sort last (issue #72).
    rows.sort_by(|a, b| {
        b.started_ms
            .cmp(&a.started_ms)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    Ok(rows)
}

/// Keep only the live sessions (`ls --live`).
pub fn only_live(rows: Vec<LsRow>) -> Vec<LsRow> {
    rows.into_iter().filter(|r| r.live).collect()
}

/// Render the `ls --live` view. Unlike [`render_table`], the empty state is
/// honest about *why* nothing printed: with recorded-but-idle sessions the
/// "run `agent-witness init`" hint would be false (ADR-0002).
pub fn render_live_table(live_rows: &[LsRow], total_sessions: usize) -> String {
    if live_rows.is_empty() && total_sessions > 0 {
        return format!(
            "No live sessions right now ({total_sessions} recorded). `agent-witness ls` lists them all.\n"
        );
    }
    render_table(live_rows)
}

/// Render rows as an aligned, deterministic text table.
pub fn render_table(rows: &[LsRow]) -> String {
    if rows.is_empty() {
        return "No sessions recorded yet. Run `agent-witness init`, then start a session.\n"
            .to_string();
    }

    // Materialize each cell as a string, then size columns to the widest cell.
    let cells: Vec<[String; COLS]> = rows
        .iter()
        .map(|r| {
            [
                r.session_id.clone(),
                if r.live { STATE_LIVE } else { STATE_IDLE }.to_string(),
                r.started_ms
                    .map(format_utc)
                    .unwrap_or_else(|| ABSENT.into()),
                r.events.to_string(),
                r.tools.to_string(),
                r.duration_ms
                    .map(format_duration_ms)
                    .unwrap_or_else(|| ABSENT.into()),
                r.corrupt.to_string(),
            ]
        })
        .collect();

    let mut widths = HEADERS.map(str::len);
    for row in &cells {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    push_row(&mut out, &HEADERS.map(String::from), &widths);
    for row in &cells {
        push_row(&mut out, row, &widths);
    }
    out
}

/// Render the default `ls` view. Unless `show_all`, sessions with no tool
/// activity (a lone `SessionStart`, or a started-then-idle stub — nothing to
/// audit) are hidden and a footer discloses how many. Honesty (ADR-0002): data
/// is never dropped silently, and `--all` always shows everything (issue #73).
pub fn render_default(rows: &[LsRow], show_all: bool) -> String {
    if show_all {
        return render_table(rows);
    }
    let active: Vec<LsRow> = rows.iter().filter(|r| r.tools > 0).cloned().collect();
    let hidden = rows.len() - active.len();

    if active.is_empty() {
        // "Nothing recorded" and "everything empty" are different states; only the
        // former should suggest `init`.
        if rows.is_empty() {
            return render_table(rows);
        }
        return format!(
            "No sessions with tool activity ({hidden} recorded with none). \
             Run `agent-witness ls --all` to show them.\n"
        );
    }

    let mut out = render_table(&active);
    if hidden > 0 {
        out.push_str(&format!(
            "\n{hidden} session(s) with no tool activity hidden — \
             run `agent-witness ls --all` to show.\n"
        ));
    }
    out
}

/// Append one padded, gap-separated row (trailing whitespace trimmed).
fn push_row(out: &mut String, cells: &[String; COLS], widths: &[usize; COLS]) {
    let line: Vec<String> = cells
        .iter()
        .zip(widths.iter())
        .map(|(cell, width)| format!("{cell:<width$}"))
        .collect();
    out.push_str(line.join(COL_GAP).trim_end());
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_witness_core::{
        AgentEvent, Attribution, EventKind, Source, CONFIDENCE_CERTAIN, DEFAULT_LIVE_WINDOW_MS,
    };
    use serde_json::json;
    use tempfile::TempDir;

    const NOW: i64 = 1_700_000_100_000;
    const CREATED: i64 = 1_700_000_000_000;

    fn row(id: &str, live: bool) -> LsRow {
        LsRow {
            session_id: id.into(),
            live,
            started_ms: Some(CREATED),
            events: 3,
            tools: 1,
            duration_ms: Some(142),
            corrupt: 0,
        }
    }

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

    #[test]
    fn empty_rows_render_a_friendly_hint() {
        let table = render_table(&[]);
        assert!(table.contains("No sessions recorded yet"));
    }

    #[test]
    fn table_has_state_column_with_live_and_idle() {
        let rows = vec![row("sess-a", true), row("sess-b", false)];
        let table = render_table(&rows);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3); // header + 2 rows
        assert!(lines[0].starts_with("SESSION"));
        assert!(lines[0].contains("STATE"));
        assert!(lines[0].contains("CORRUPT"));
        assert!(lines[1].contains("sess-a"));
        assert!(lines[1].contains("live"));
        assert!(lines[1].contains("2023-11-14 22:13:20Z"));
        assert!(lines[2].contains("sess-b"));
        assert!(lines[2].contains("idle"));
        // Corrupt count is still the last column.
        assert!(lines[2].trim_end().ends_with('0'));
    }

    #[test]
    fn live_view_empty_state_is_honest_about_recorded_sessions() {
        // 3 recorded, none live: must NOT claim nothing was ever recorded.
        let out = render_live_table(&[], 3);
        assert!(out.contains("No live sessions right now (3 recorded)"));
        assert!(!out.contains("agent-witness init"));
        // Truly nothing recorded: the init hint is correct.
        let out = render_live_table(&[], 0);
        assert!(out.contains("No sessions recorded yet"));
    }

    #[test]
    fn live_view_renders_rows_like_the_plain_table() {
        let rows = vec![row("live-1", true)];
        assert_eq!(render_live_table(&rows, 5), render_table(&rows));
    }

    #[test]
    fn only_live_keeps_running_sessions() {
        let rows = vec![
            row("live-1", true),
            row("idle-1", false),
            row("live-2", true),
        ];
        let live = only_live(rows);
        assert_eq!(live.len(), 2);
        assert!(live.iter().all(|r| r.live));
    }

    #[test]
    fn collect_rows_marks_live_and_stopped_sessions() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        // Live: started, recent, no stop.
        let mut w = store.open("live-sess", CREATED).unwrap();
        w.append(&event(NOW - 1_000, EventKind::SessionStart))
            .unwrap();
        w.append(&event(NOW - 500, EventKind::ToolCall)).unwrap();
        drop(w);

        // Stopped: started, recent, but has a Stop.
        let mut w = store.open("stopped-sess", CREATED).unwrap();
        w.append(&event(NOW - 1_000, EventKind::SessionStart))
            .unwrap();
        w.append(&event(NOW - 500, EventKind::Stop)).unwrap();
        drop(w);

        // Stale: started, no stop, but last activity is beyond the window.
        let mut w = store.open("stale-sess", CREATED).unwrap();
        w.append(&event(
            NOW - DEFAULT_LIVE_WINDOW_MS - 10,
            EventKind::SessionStart,
        ))
        .unwrap();
        drop(w);

        let rows = collect_rows(&store, NOW, DEFAULT_LIVE_WINDOW_MS).unwrap();
        let live_of = |id: &str| rows.iter().find(|r| r.session_id == id).unwrap().live;
        assert!(live_of("live-sess"));
        assert!(!live_of("stopped-sess"));
        assert!(!live_of("stale-sess"));

        assert_eq!(only_live(rows).len(), 1);
    }

    #[test]
    fn collect_rows_orders_most_recent_first_then_by_id() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        // Opened out of order; `created_ts` sets the start. Two share a start to
        // exercise the id tie-break.
        for (id, created) in [
            ("bbb", CREATED + 2_000),
            ("aaa", CREATED + 1_000),
            ("ccc", CREATED + 3_000),
            ("dup2", CREATED + 5_000),
            ("dup1", CREATED + 5_000),
        ] {
            let mut w = store.open(id, created).unwrap();
            w.append(&event(created, EventKind::SessionStart)).unwrap();
            drop(w);
        }

        let rows = collect_rows(&store, NOW, DEFAULT_LIVE_WINDOW_MS).unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.session_id.as_str()).collect();
        // Newest start first; equal starts break by id ascending.
        assert_eq!(ids, vec!["dup1", "dup2", "ccc", "bbb", "aaa"]);
    }

    /// A row with no tool activity (the empty-stub shape hidden by default).
    fn empty_row(id: &str) -> LsRow {
        LsRow {
            tools: 0,
            events: 1,
            ..row(id, false)
        }
    }

    #[test]
    fn render_default_hides_empty_sessions_and_discloses_count() {
        let rows = vec![
            row("active-a", false),
            empty_row("empty-a"),
            empty_row("empty-b"),
        ];
        let out = render_default(&rows, false);
        assert!(out.contains("active-a"));
        assert!(!out.contains("empty-a"));
        assert!(!out.contains("empty-b"));
        // The hidden count is disclosed, never silent.
        assert!(out.contains("2 session(s) with no tool activity hidden"));
        assert!(out.contains("--all"));
    }

    #[test]
    fn render_default_all_shows_every_session_without_footer() {
        let rows = vec![row("active-a", false), empty_row("empty-a")];
        let out = render_default(&rows, true);
        assert!(out.contains("active-a"));
        assert!(out.contains("empty-a"));
        assert!(!out.contains("hidden"));
    }

    #[test]
    fn render_default_all_empty_states_it_without_the_init_hint() {
        let rows = vec![empty_row("empty-a"), empty_row("empty-b")];
        let out = render_default(&rows, false);
        assert!(out.contains("No sessions with tool activity"));
        assert!(out.contains("2 recorded with none"));
        // There ARE sessions, so the `init` hint would be false (ADR-0002).
        assert!(!out.contains("agent-witness init"));
    }
}
