//! Fixture-driven integration tests for the hooks receiver (issue #2).
//!
//! Runs the real #8 fixtures through BOTH ingestion paths — the `emit` fallback
//! (no daemon) and the socket path (`watch` server) — and asserts the resulting
//! JSONL store matches. Also covers malformed-JSON resilience.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent_witness::{emit, watch};
use agent_witness_core::{Attribution, Clock, EventKind, FixedClock, SessionStore, Source};
use tokio::sync::oneshot;

const FIXED_TS: i64 = 1_700_000_000_000;
/// Upper bound for the socket-path poll loop before we declare a hang.
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Repo `tests/fixtures/<scenario>/hooks.jsonl` for a scenario.
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

/// Expected normalized kinds per fixture, in file order.
fn expected_kinds(scenario: &str) -> Vec<EventKind> {
    match scenario {
        "session-basic" => vec![
            EventKind::SessionStart,
            EventKind::Prompt,
            EventKind::ToolCall,   // Write PreToolUse
            EventKind::ToolResult, // Write PostToolUse
            EventKind::ToolCall,   // Bash PreToolUse
            EventKind::ToolResult, // Bash PostToolUse
            EventKind::Stop,
        ],
        "session-with-failure" => vec![
            EventKind::SessionStart,
            EventKind::Prompt,
            EventKind::ToolCall,   // failed `cat` — unpaired, the failure signal
            EventKind::ToolCall,   // `ls` PreToolUse
            EventKind::ToolResult, // `ls` PostToolUse
            EventKind::Stop,
        ],
        other => panic!("unknown scenario {other}"),
    }
}

/// Every fixture line carries `session_id` equal to the scenario name.
const SCENARIOS: &[&str] = &["session-basic", "session-with-failure"];

fn assert_store_matches_fixture(store: &SessionStore, scenario: &str) {
    let read = store.read(scenario).expect("read session");
    assert_eq!(
        read.skipped_lines, 0,
        "{scenario}: unexpected skipped lines"
    );

    let kinds: Vec<_> = read.events.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, expected_kinds(scenario), "{scenario}: event kinds");

    // Every hook-derived event is Direct, from Hooks, and linked to a raw record.
    for ev in &read.events {
        assert_eq!(ev.attribution, Attribution::Direct, "{scenario}");
        assert_eq!(ev.source, Source::Hooks, "{scenario}");
        assert!(
            ev.raw_event_ref.is_some(),
            "{scenario}: missing raw_event_ref"
        );
    }

    // Raw records preserved 1:1 and verbatim (canonical source, ADR-0001).
    let raw = store.read_raw(scenario).expect("read raw");
    assert_eq!(raw.skipped_lines, 0, "{scenario}: skipped raw lines");
    let fixture = fixture_lines(scenario);
    assert_eq!(
        raw.records.len(),
        fixture.len(),
        "{scenario}: raw record count"
    );
    for (rec, line) in raw.records.iter().zip(fixture.iter()) {
        assert_eq!(&rec.raw, line, "{scenario}: raw not verbatim");
    }

    // The raw_ref on each event must resolve to a stored raw record.
    for ev in &read.events {
        let want = ev.raw_event_ref.as_deref().unwrap();
        assert!(
            raw.records.iter().any(|r| r.raw_ref == want),
            "{scenario}: dangling raw_event_ref {want}"
        );
    }
}

#[tokio::test]
async fn fallback_path_records_all_fixtures() {
    for scenario in SCENARIOS {
        let tmp = tempfile::TempDir::new().unwrap();
        let sessions_root = tmp.path().to_path_buf();
        // A socket path that does not exist forces the fallback.
        let absent_socket = tmp.path().join("no-such.sock");

        for line in fixture_lines(scenario) {
            let outcome = emit::run_emit(&absent_socket, &sessions_root, line)
                .await
                .expect("emit fallback");
            assert_eq!(outcome, emit::EmitOutcome::Fallback, "{scenario}");
        }

        assert_store_matches_fixture(&SessionStore::new(&sessions_root), scenario);
    }
}

#[tokio::test]
async fn socket_path_records_all_fixtures() {
    for scenario in SCENARIOS {
        let tmp = tempfile::TempDir::new().unwrap();
        let sessions_root = tmp.path().join("sessions");
        let socket = tmp.path().join("witness.sock");

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let clock: Arc<dyn Clock + Send + Sync> = Arc::new(FixedClock(FIXED_TS));
        let server = {
            let socket = socket.clone();
            let sessions_root = sessions_root.clone();
            tokio::spawn(async move {
                watch::run_watch(&socket, &sessions_root, clock, async {
                    let _ = shutdown_rx.await;
                })
                .await
            })
        };

        wait_for_socket(&socket).await;

        let lines = fixture_lines(scenario);
        for line in &lines {
            let outcome = emit::run_emit(&socket, &sessions_root, line.clone())
                .await
                .expect("emit forward");
            assert_eq!(outcome, emit::EmitOutcome::Forwarded, "{scenario}");
        }

        // The daemon acks only after persistence, so once every emit returned
        // Forwarded the store is already complete — no polling needed.
        let store = SessionStore::new(&sessions_root);

        let _ = shutdown_tx.send(());
        server.await.unwrap().unwrap();

        assert_store_matches_fixture(&store, scenario);
    }
}

#[tokio::test]
async fn no_ack_from_daemon_triggers_fallback() {
    // A "daemon" that accepts and closes without persisting or acking — e.g.
    // a listener that dies mid-request. emit must not report success and must
    // write the record itself.
    let tmp = tempfile::TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    let socket = tmp.path().join("dead.sock");

    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let acceptor = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            drop(stream); // close immediately: no read, no ack
        }
    });

    let line = fixture_lines("session-basic").remove(0);
    let outcome = emit::run_emit(&socket, &sessions_root, line.clone())
        .await
        .expect("emit must fall back, not fail");
    assert_eq!(outcome, emit::EmitOutcome::Fallback);
    acceptor.await.unwrap();

    // The record landed in the store via the fallback.
    let store = SessionStore::new(&sessions_root);
    let raw = store.read_raw("session-basic").unwrap();
    assert_eq!(raw.records.len(), 1);
    assert_eq!(raw.records[0].raw, line);
}

#[tokio::test]
async fn malformed_json_is_recorded_not_crashed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sessions_root = tmp.path().to_path_buf();
    let absent_socket = tmp.path().join("no-such.sock");

    let outcome = emit::run_emit(
        &absent_socket,
        &sessions_root,
        "{ not json at all".to_string(),
    )
    .await
    .expect("emit must not crash on malformed input");
    assert_eq!(outcome, emit::EmitOutcome::Fallback);

    // Landed in the unknown-session bucket as an Error event.
    let store = SessionStore::new(&sessions_root);
    let read = store.read(agent_witness_core::UNKNOWN_SESSION).unwrap();
    assert_eq!(read.events.len(), 1);
    assert_eq!(read.events[0].kind, EventKind::Error);
    // Raw bytes still preserved verbatim.
    let raw = store.read_raw(agent_witness_core::UNKNOWN_SESSION).unwrap();
    assert_eq!(raw.records[0].raw, "{ not json at all");
}

/// Wait until the server has created and bound its socket file.
async fn wait_for_socket(socket: &Path) {
    let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
    loop {
        if socket.exists() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("socket {} was never created", socket.display());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
