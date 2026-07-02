//! Integration tests for the transcript adapter wiring (issue #4).
//!
//! Drives the real `emit`/`watch` code paths with a `Stop` hook that points at a
//! temporary transcript file, and asserts:
//! - ON (default): supplementary `Observed` transcript events land alongside the
//!   canonical hook events;
//! - OFF (`--no-transcript`): recording is hooks-only;
//! - a missing/unreadable transcript never breaks hook recording (isolation).

use std::path::Path;
use std::sync::Arc;

use agent_witness::{emit, watch};
use agent_witness_core::{Attribution, Clock, FixedClock, SessionStore, Source};
use tokio::sync::oneshot;

const TRANSCRIPT: bool = true;
const NO_TRANSCRIPT: bool = false;

/// Two assistant lines: one with prose (extracted) and one tool-only (skipped).
const TRANSCRIPT_BODY: &str = concat!(
    r#"{"type":"assistant","uuid":"tx-1","timestamp":"2026-07-02T02:36:29.167Z","message":{"content":[{"type":"text","text":"working on it"}]}}"#,
    "\n",
    r#"{"type":"assistant","uuid":"tx-2","message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}"#,
);

fn stop_payload(session: &str, transcript_path: &Path) -> String {
    format!(
        r#"{{"hook_event_name":"Stop","session_id":"{session}","stop_hook_active":false,"transcript_path":"{}"}}"#,
        transcript_path.display()
    )
}

fn write_transcript(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("transcript.jsonl");
    std::fs::write(&path, TRANSCRIPT_BODY).unwrap();
    path
}

#[tokio::test]
async fn fallback_path_with_transcript_on_appends_observed_events() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    let absent_socket = tmp.path().join("no-such.sock");
    let transcript = write_transcript(tmp.path());
    let payload = stop_payload("s-on", &transcript);

    let outcome = emit::run_emit(&absent_socket, &sessions_root, payload, TRANSCRIPT)
        .await
        .unwrap();
    assert_eq!(outcome, emit::EmitOutcome::Fallback);

    let events = SessionStore::new(&sessions_root)
        .read("s-on")
        .unwrap()
        .events;
    // One Stop (Hooks) + one assistant text (Transcript).
    assert_eq!(events.len(), 2);
    let transcript_events: Vec<_> = events
        .iter()
        .filter(|e| e.source == Source::Transcript)
        .collect();
    assert_eq!(transcript_events.len(), 1);
    assert_eq!(transcript_events[0].attribution, Attribution::Observed);
    assert_eq!(transcript_events[0].payload["text"], "working on it");
}

#[tokio::test]
async fn fallback_path_with_transcript_off_is_hooks_only() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    let absent_socket = tmp.path().join("no-such.sock");
    let transcript = write_transcript(tmp.path());
    let payload = stop_payload("s-off", &transcript);

    // Transcript file EXISTS, but the flag is off: it must be ignored entirely.
    emit::run_emit(&absent_socket, &sessions_root, payload, NO_TRANSCRIPT)
        .await
        .unwrap();

    let events = SessionStore::new(&sessions_root)
        .read("s-off")
        .unwrap()
        .events;
    assert_eq!(events.len(), 1, "only the hook event should be recorded");
    assert_eq!(events[0].source, Source::Hooks);
    assert!(events.iter().all(|e| e.source != Source::Transcript));
}

#[tokio::test]
async fn missing_transcript_file_does_not_break_hook_recording() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    let absent_socket = tmp.path().join("no-such.sock");
    // Point at a path that does not exist; transcript ON.
    let missing = tmp.path().join("nope.jsonl");
    let payload = stop_payload("s-miss", &missing);

    let outcome = emit::run_emit(&absent_socket, &sessions_root, payload, TRANSCRIPT)
        .await
        .expect("missing transcript must not fail the hook");
    assert_eq!(outcome, emit::EmitOutcome::Fallback);

    let events = SessionStore::new(&sessions_root)
        .read("s-miss")
        .unwrap()
        .events;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source, Source::Hooks);
}

#[tokio::test]
async fn watch_path_ingests_transcript_on_stop() {
    let tmp = tempfile::TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    let socket = tmp.path().join("witness.sock");
    let transcript = write_transcript(tmp.path());
    let payload = stop_payload("s-watch", &transcript);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let clock: Arc<dyn Clock + Send + Sync> = Arc::new(FixedClock(1_700_000_000_000));
    let server = {
        let socket = socket.clone();
        let sessions_root = sessions_root.clone();
        tokio::spawn(async move {
            watch::run_watch(&socket, &sessions_root, clock, TRANSCRIPT, async {
                let _ = shutdown_rx.await;
            })
            .await
        })
    };

    wait_for_socket(&socket).await;
    let outcome = emit::run_emit(&socket, &sessions_root, payload, NO_TRANSCRIPT)
        .await
        .unwrap();
    // emit forwards; the daemon owns transcript ingest here.
    assert_eq!(outcome, emit::EmitOutcome::Forwarded);

    let _ = shutdown_tx.send(());
    server.await.unwrap().unwrap();

    let events = SessionStore::new(&sessions_root)
        .read("s-watch")
        .unwrap()
        .events;
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|e| e.source == Source::Transcript)
            .count(),
        1
    );
}

async fn wait_for_socket(socket: &Path) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if socket.exists() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("socket {} was never created", socket.display());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
