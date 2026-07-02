//! Golden-screen tests for the `top` resident view (issue #19).
//!
//! Same determinism guarantee as the timeline/pick golden tests: rendering is a
//! pure function of the [`TopApp`] state, so a fixed-size `TestBackend` yields a
//! byte-for-byte stable screen. The drilldown case renders the timeline viewer a
//! chosen row drops into, proving `top` → `show` reuses the same timeline.
//! Regenerate after an intentional UI change with:
//!
//! ```text
//! UPDATE_GOLDEN=1 cargo nextest run -p agent-witness top_golden
//! ```

use std::path::{Path, PathBuf};

use agent_witness::top::{render_top, TopApp, TopRow};
use agent_witness::tui::{render as render_tui, TuiApp};
use agent_witness_core::{normalize, AgentEvent, SessionRead};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

/// Fixed golden screen size: output must not depend on terminal size.
const GOLDEN_WIDTH: u16 = 80;
const GOLDEN_HEIGHT: u16 = 24;
/// Stable base timestamp (2023-11-14 22:13:20Z) for readable golden output.
const BASE_TS: i64 = 1_700_000_000_000;
const TS_STEP: i64 = 1_000;

fn render_top_to_string(app: &TopApp) -> String {
    let backend = TestBackend::new(GOLDEN_WIDTH, GOLDEN_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|frame| render_top(frame, app)).expect("draw");
    buffer_to_string(terminal.backend().buffer())
}

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

fn sample_rows() -> Vec<TopRow> {
    vec![
        TopRow {
            session_id: "agent-witness".to_string(),
            project: "agent-witness".to_string(),
            running_tool: Some("Bash".to_string()),
            last_activity_age_ms: Some(2_000),
            elapsed_ms: Some(125_000),
            events: 42,
        },
        TopRow {
            session_id: "avvy-core-9f8c".to_string(),
            project: "avvy-core".to_string(),
            running_tool: Some("Edit".to_string()),
            last_activity_age_ms: Some(15_000),
            elapsed_ms: Some(600_000),
            events: 9,
        },
        TopRow {
            session_id: "idle-thinking".to_string(),
            project: "docs".to_string(),
            running_tool: None,
            last_activity_age_ms: Some(45_000),
            elapsed_ms: Some(90_000),
            events: 3,
        },
    ]
}

#[test]
fn golden_top_zero_sessions() {
    let app = TopApp::new(vec![]);
    let screen = render_top_to_string(&app);
    assert!(screen.contains("0 live session(s)"));
    assert!(screen.contains("(no live sessions right now)"));
    assert_eq!(screen.lines().count() as u16, GOLDEN_HEIGHT);
    assert_golden("top_zero", &screen);
}

#[test]
fn golden_top_multiple_sessions() {
    // Second row selected so the highlight and a running tool are both visible.
    let mut app = TopApp::new(sample_rows());
    app.selected = 1;
    let screen = render_top_to_string(&app);
    assert!(screen.contains("3 live session(s)"));
    assert!(screen.contains("Bash"));
    assert!(screen.contains("Edit"));
    assert_golden("top_multiple", &screen);
}

#[test]
fn golden_top_drilldown_opens_the_timeline() {
    // Pressing Enter on a top row drills into that session's timeline; this is
    // the destination screen, rendered via the very same tui::render as `show`.
    let events = fixture_events("session-basic");
    let app = TuiApp::new(
        "agent-witness",
        &SessionRead {
            events,
            skipped_lines: 0,
        },
    );
    let backend = TestBackend::new(GOLDEN_WIDTH, GOLDEN_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| render_tui(frame, &app))
        .expect("draw");
    let screen = buffer_to_string(terminal.backend().buffer());
    assert!(screen.contains("timeline"));
    assert_golden("top_drilldown", &screen);
}

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
