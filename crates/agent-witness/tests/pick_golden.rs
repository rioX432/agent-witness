//! Golden-screen test for the `show --pick` session picker (issue #18).
//!
//! Same determinism guarantee as the timeline golden tests: rendering is a pure
//! function of the [`PickApp`] state, so a fixed-size `TestBackend` yields a
//! byte-for-byte stable screen. Regenerate after an intentional UI change with:
//!
//! ```text
//! UPDATE_GOLDEN=1 cargo nextest run -p agent-witness pick_golden
//! ```

use std::path::{Path, PathBuf};

use agent_witness::pick::{render_pick, PickApp, PickRow};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

/// Fixed golden screen size: output must not depend on terminal size.
const GOLDEN_WIDTH: u16 = 80;
const GOLDEN_HEIGHT: u16 = 24;
/// A stable base timestamp (2023-11-14 22:13:20Z) for readable golden output.
const BASE_TS: i64 = 1_700_000_000_000;

fn render_to_string(app: &PickApp) -> String {
    let backend = TestBackend::new(GOLDEN_WIDTH, GOLDEN_HEIGHT);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| render_pick(frame, app))
        .expect("draw");
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

fn sample_rows() -> Vec<PickRow> {
    vec![
        PickRow {
            session_id: "session-live".to_string(),
            started_ms: Some(BASE_TS + 60_000),
            events: 4,
            tools: 2,
            live: true,
        },
        PickRow {
            session_id: "session-basic".to_string(),
            started_ms: Some(BASE_TS),
            events: 7,
            tools: 2,
            live: false,
        },
        PickRow {
            session_id: "session-with-failure".to_string(),
            started_ms: None,
            events: 0,
            tools: 0,
            live: false,
        },
    ]
}

#[test]
fn golden_pick_list() {
    // Second row selected so the highlight and the live marker both appear.
    let mut app = PickApp::new(sample_rows());
    app.selected = 1;
    let screen = render_to_string(&app);
    assert!(
        screen.contains("live"),
        "live marker must be visible:\n{screen}"
    );
    assert_golden("pick_list", &screen);
}

#[test]
fn empty_pick_list_renders_without_breakage() {
    let app = PickApp::new(vec![]);
    let screen = render_to_string(&app);
    assert!(screen.contains("0 session(s) recorded"));
    assert!(screen.contains("(no sessions recorded yet)"));
    assert_eq!(screen.lines().count() as u16, GOLDEN_HEIGHT);
}
