//! `agent-witness ls`: a plain-stdout listing of recorded sessions.
//!
//! Non-TUI on purpose (the issue's 実装方針): a simple aligned table is the
//! right shape for piping and for quick scanning. The `CORRUPT` column is the
//! honesty surface here — corrupted/unparseable lines are counted and shown,
//! never hidden (ADR-0002).

use agent_witness_core::SessionStore;
use anyhow::Result;

use crate::timefmt::{format_duration_ms, format_utc};
use crate::timeline::tool_call_count;

/// A single session's summary row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LsRow {
    /// Session id (directory name).
    pub session_id: String,
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
const HEADERS: [&str; 6] = [
    "SESSION", "STARTED", "EVENTS", "TOOLS", "DURATION", "CORRUPT",
];
/// Gap between columns.
const COL_GAP: &str = "  ";
/// Placeholder for an absent value.
const ABSENT: &str = "-";

/// Gather a summary row for every recorded session, sorted by id.
pub fn collect_rows(store: &SessionStore) -> Result<Vec<LsRow>> {
    let mut rows = Vec::new();
    for id in store.list_sessions()? {
        let read = store.read(&id)?;
        let first_ts = read.events.first().map(|e| e.ts);
        let last_ts = read.events.last().map(|e| e.ts);
        // Prefer the recorded session start; fall back to the first event's time
        // for sessions whose meta is missing.
        let started_ms = store.read_meta(&id).ok().map(|m| m.created_ts).or(first_ts);
        let duration_ms = match (first_ts, last_ts) {
            (Some(first), Some(last)) => Some(last - first),
            _ => None,
        };
        rows.push(LsRow {
            session_id: id,
            started_ms,
            events: read.events.len(),
            tools: tool_call_count(&read.events),
            duration_ms,
            corrupt: read.skipped_lines,
        });
    }
    Ok(rows)
}

/// Render rows as an aligned, deterministic text table.
pub fn render_table(rows: &[LsRow]) -> String {
    if rows.is_empty() {
        return "No sessions recorded yet. Run `agent-witness init`, then start a session.\n"
            .to_string();
    }

    // Materialize each cell as a string, then size columns to the widest cell.
    let cells: Vec<[String; 6]> = rows
        .iter()
        .map(|r| {
            [
                r.session_id.clone(),
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

/// Append one padded, gap-separated row (trailing whitespace trimmed).
fn push_row(out: &mut String, cells: &[String; 6], widths: &[usize; 6]) {
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

    #[test]
    fn empty_rows_render_a_friendly_hint() {
        let table = render_table(&[]);
        assert!(table.contains("No sessions recorded yet"));
    }

    #[test]
    fn table_has_header_and_one_line_per_row() {
        let rows = vec![
            LsRow {
                session_id: "sess-a".into(),
                started_ms: Some(1_700_000_000_000),
                events: 7,
                tools: 2,
                duration_ms: Some(142),
                corrupt: 0,
            },
            LsRow {
                session_id: "sess-b".into(),
                started_ms: None,
                events: 0,
                tools: 0,
                duration_ms: None,
                corrupt: 1,
            },
        ];
        let table = render_table(&rows);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3); // header + 2 rows
        assert!(lines[0].starts_with("SESSION"));
        assert!(lines[0].contains("CORRUPT"));
        assert!(lines[1].contains("sess-a"));
        assert!(lines[1].contains("2023-11-14 22:13:20Z"));
        // Absent values render as the placeholder, corrupt count is visible.
        assert!(lines[2].contains("sess-b"));
        assert!(lines[2].trim_end().ends_with('1'));
    }
}
