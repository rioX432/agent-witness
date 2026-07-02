//! Integration test for `show --follow` live tail (issue #19).
//!
//! Acceptance criterion: a separate task appends hook events via `emit` and the
//! follow reader picks them up. This drives the real store read + refresh path
//! ([`agent_witness::tui::poll_update`], the same step the TUI run loop uses),
//! so the test proves live tail without needing a terminal.

use std::path::Path;
use std::time::Duration;

use agent_witness::emit;
use agent_witness::tui::{poll_update, TuiApp};
use agent_witness_core::SessionStore;

/// This test exercises the hooks pipeline only; the transcript adapter is
/// covered elsewhere.
const NO_TRANSCRIPT: bool = false;
/// Upper bound before we declare the follow reader hung.
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Poll cadence for the follow reader in the test.
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// How long the appender pauses between emits, to interleave with polling.
const APPEND_GAP: Duration = Duration::from_millis(10);

/// Repo `tests/fixtures/<scenario>/hooks.jsonl` lines for a scenario.
fn fixture_lines(scenario: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join(scenario)
        .join("hooks.jsonl");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn follow_reader_picks_up_appends_from_a_separate_task() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sessions_root = tmp.path().to_path_buf();
    // A non-existent socket forces emit's direct-store fallback (no daemon).
    let absent_socket = tmp.path().join("no-such.sock");

    let session = "session-basic";
    let lines = fixture_lines(session);
    let seed = 2; // SessionStart + Prompt
    assert!(lines.len() > seed, "fixture must have events to append");

    // Seed the store, then build a following viewer over it.
    for line in &lines[..seed] {
        emit::run_emit(&absent_socket, &sessions_root, line.clone(), NO_TRANSCRIPT)
            .await
            .expect("seed emit");
    }
    let store = SessionStore::new(&sessions_root);
    let mut app = TuiApp::new(session, &store.read(session).expect("read seed"));
    app.follow = true;
    let initial = app.event_count;
    assert_eq!(initial, seed);

    // A separate task appends the remaining events via emit — a stand-in for
    // another process recording the session live.
    let rest: Vec<String> = lines[seed..].to_vec();
    let root = sessions_root.clone();
    let socket = absent_socket.clone();
    let appender = tokio::spawn(async move {
        for line in rest {
            emit::run_emit(&socket, &root, line, NO_TRANSCRIPT)
                .await
                .expect("append emit");
            tokio::time::sleep(APPEND_GAP).await;
        }
    });

    // The follow reader polls until it observes every appended event.
    let mut observed_update = false;
    let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
    while app.event_count < lines.len() {
        if poll_update(&mut app, &store, session).expect("poll") {
            observed_update = true;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "follow reader never caught up: saw {} of {} events",
                app.event_count,
                lines.len()
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    appender.await.expect("appender task");

    assert!(observed_update, "reader must have refreshed at least once");
    assert_eq!(app.event_count, lines.len(), "all appends reflected");
    assert!(app.event_count > initial);

    // Once caught up, a further poll with no new data is a no-op.
    assert!(
        !poll_update(&mut app, &store, session).expect("idle poll"),
        "no update expected when the log did not grow"
    );
}
