//! Golden-screen tests for the TUI timeline (issue #5).
//!
//! Determinism is the verification point (docs/test-strategy.md): rendering is a
//! pure function of the event data, so a fixed-size `TestBackend` yields a
//! byte-for-byte stable screen. Golden files live under `tests/golden/`. To
//! regenerate them after an intentional UI change, run:
//!
//! ```text
//! UPDATE_GOLDEN=1 cargo nextest run -p agent-witness golden
//! ```
//!
//! The robustness cases (empty / huge payload / very long line) assert the
//! layout never breaks — no panic, dimensions unchanged — as required by the
//! issue's acceptance criteria.

use std::path::{Path, PathBuf};

use agent_witness::tui::{render, TuiApp};
use agent_witness_core::{normalize, AgentEvent, SessionRead};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use serde_json::json;

/// Fixed golden screen size: golden output must not depend on terminal size.
const GOLDEN_WIDTH: u16 = 80;
const GOLDEN_HEIGHT: u16 = 24;
/// Base timestamp for normalized fixtures (2023-11-14 22:13:20Z). Events are
/// spaced a fixed step apart so relative offsets are stable and readable.
const BASE_TS: i64 = 1_700_000_000_000;
const TS_STEP: i64 = 1_000;

/// Normalize a fixture's `hooks.jsonl` into events with deterministic times.
fn fixture_events(scenario: &str) -> Vec<AgentEvent> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join(scenario)
        .join("hooks.jsonl");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            let value = serde_json::from_str(line).expect("fixture line is valid json");
            let ts = BASE_TS + (i as i64) * TS_STEP;
            normalize(&value, ts, &format!("raw-{i}")).expect("fixture line normalizes")
        })
        .collect()
}

/// Render an app to a fixed-size buffer and return the screen text.
fn render_to_string(app: &TuiApp) -> String {
    let backend = TestBackend::new(GOLDEN_WIDTH, GOLDEN_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|frame| render(frame, app)).expect("draw");
    buffer_to_string(terminal.backend().buffer())
}

/// Flatten a rendered buffer to text: one line per row, trailing spaces trimmed.
/// Styles are intentionally ignored — content is what the golden asserts.
fn buffer_to_string(buffer: &Buffer) -> String {
    let area = buffer.area();
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Compare `actual` against the golden file, or (re)write it under UPDATE_GOLDEN.
fn assert_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).expect("create golden dir");
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {}: {e}\nRun with UPDATE_GOLDEN=1 to create it",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "golden mismatch for {name}; run UPDATE_GOLDEN=1 to update if intended"
    );
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(format!("{name}.txt"))
}

fn read(events: Vec<AgentEvent>, skipped: usize) -> SessionRead {
    SessionRead {
        events,
        skipped_lines: skipped,
    }
}

#[test]
fn golden_basic_timeline() {
    let app = TuiApp::new("session-basic", &read(fixture_events("session-basic"), 0));
    assert_golden("basic_timeline", &render_to_string(&app));
}

#[test]
fn golden_basic_detail_open() {
    let mut app = TuiApp::new("session-basic", &read(fixture_events("session-basic"), 0));
    // Select the first tool call (the Write) and open its detail pane.
    app.selected = 2;
    app.detail_open = true;
    assert_golden("basic_detail_open", &render_to_string(&app));
}

#[test]
fn golden_failure_timeline_shows_no_result() {
    let app = TuiApp::new(
        "session-with-failure",
        &read(fixture_events("session-with-failure"), 0),
    );
    let screen = render_to_string(&app);
    // The unpaired (failed) Bash call must be visibly labelled, not assumed ok.
    assert!(
        screen.contains("no-result"),
        "failure screen must surface the unpaired call:\n{screen}"
    );
    assert_golden("failure_timeline", &screen);
}

#[test]
fn empty_session_renders_without_breakage() {
    let app = TuiApp::new("empty", &read(vec![], 0));
    let screen = render_to_string(&app);
    assert!(screen.contains("0 events"));
    assert!(screen.contains("(no events recorded yet)"));
    // Placeholder guards against a detail pane on an empty session.
    assert_eq!(screen.lines().count() as u16, GOLDEN_HEIGHT);
}

#[test]
fn huge_payload_does_not_break_layout() {
    // A single event whose payload is enormous must not panic or overflow.
    let big = "x".repeat(200_000);
    let event = normalize(
        &json!({
            "hook_event_name": "PreToolUse",
            "session_id": "huge",
            "tool_name": "Write",
            "tool_use_id": "t1",
            "tool_input": {"file_path": "/tmp/big", "content": big},
        }),
        BASE_TS,
        "raw-0",
    )
    .expect("normalizes");

    let mut app = TuiApp::new("huge", &read(vec![event], 0));
    app.detail_open = true; // force the huge payload through the detail pane too
    let backend = TestBackend::new(GOLDEN_WIDTH, GOLDEN_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|frame| render(frame, &app)).expect("draw");
    // Dimensions unchanged: no overflow, no wrap-induced resize.
    let area = terminal.backend().buffer().area();
    assert_eq!((area.width, area.height), (GOLDEN_WIDTH, GOLDEN_HEIGHT));
}

#[test]
fn very_long_single_line_is_clipped_not_broken() {
    // A one-line command longer than the screen width must be clipped to the
    // pane, leaving the row count (and thus layout) intact.
    let long_cmd = format!("echo {}", "a".repeat(5_000));
    let event = normalize(
        &json!({
            "hook_event_name": "PreToolUse",
            "session_id": "long",
            "tool_name": "Bash",
            "tool_use_id": "t1",
            "tool_input": {"command": long_cmd},
        }),
        BASE_TS,
        "raw-0",
    )
    .expect("normalizes");

    let app = TuiApp::new("long", &read(vec![event], 0));
    let screen = render_to_string(&app);
    assert_eq!(screen.lines().count() as u16, GOLDEN_HEIGHT);
    // No rendered row may exceed the fixed width.
    for line in screen.lines() {
        assert!(
            line.chars().count() <= GOLDEN_WIDTH as usize,
            "row exceeded width: {}",
            line.chars().count()
        );
    }
}

#[test]
fn corrupt_line_count_is_visible_in_header() {
    // Honesty (ADR-0002): skipped/corrupt lines are surfaced, not hidden.
    let app = TuiApp::new("session-basic", &read(fixture_events("session-basic"), 3));
    let screen = render_to_string(&app);
    assert!(
        screen.contains("3 corrupt lines"),
        "corrupt count must be visible:\n{screen}"
    );
}
