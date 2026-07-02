//! Three-state liveness canary over the real multi-turn fixture (issue #23).
//!
//! `session-multiturn` is a real capture of two assistant turns in one session:
//! its `Stop` at index 4 is followed by more events. This is the regression
//! guard for "Stop fires per-turn, not per-session": the shared liveness rule
//! must read the session as
//! - **live** while mid-turn (last event is not a `Stop`, within window),
//! - **idle** between turns (last event is a `Stop`), and
//! - **idle/stale** once the last event falls outside the window,
//!
//! and must return to **live** as soon as the next turn's first event arrives.

use agent_witness_core::{normalize, summarize, AgentEvent, DEFAULT_LIVE_WINDOW_MS};
use std::path::Path;

/// Deterministic synthetic clock for normalized events: the fixture payloads
/// carry no timestamp (the receiver injects one), so the test owns time.
const BASE_TS: i64 = 1_700_000_000_000;
/// One second between consecutive fixture events.
const TS_STEP: i64 = 1_000;

/// Index of the fixture's first `Stop` (end of turn 1), followed by more events.
const FIRST_STOP_INDEX: usize = 4;
/// Index of the last non-`Stop` event (turn 2's `PostToolUse`, mid-turn).
const MID_TURN_INDEX: usize = 8;

/// Normalize the multi-turn fixture into events with deterministic times.
fn multiturn_events() -> Vec<AgentEvent> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("session-multiturn")
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

/// Liveness of the fixture truncated to its first `end` events, evaluated `now`.
fn live_through(events: &[AgentEvent], end: usize, now_ms: i64) -> bool {
    let slice = &events[..end];
    summarize("session-multiturn", None, slice).is_live_within(now_ms, DEFAULT_LIVE_WINDOW_MS)
}

#[test]
fn fixture_shape_matches_the_canary_assumption() {
    let events = multiturn_events();
    assert_eq!(events.len(), 10, "expected a 10-event two-turn capture");
    // The load-bearing fact: a Stop mid-stream, with events after it.
    use agent_witness_core::EventKind;
    assert_eq!(events[FIRST_STOP_INDEX].kind, EventKind::Stop);
    assert_ne!(events[MID_TURN_INDEX].kind, EventKind::Stop);
}

#[test]
fn mid_turn_reads_live() {
    let events = multiturn_events();
    // Through the mid-turn PostToolUse (last event is not a Stop), seconds later.
    let last_ts = BASE_TS + (MID_TURN_INDEX as i64) * TS_STEP;
    assert!(live_through(&events, MID_TURN_INDEX + 1, last_ts + TS_STEP));
}

#[test]
fn between_turns_reads_idle() {
    let events = multiturn_events();
    // Through the first Stop (last event IS a Stop), seconds later: idle.
    let stop_ts = BASE_TS + (FIRST_STOP_INDEX as i64) * TS_STEP;
    assert!(!live_through(
        &events,
        FIRST_STOP_INDEX + 1,
        stop_ts + TS_STEP
    ));
}

#[test]
fn stale_past_window_reads_idle() {
    let events = multiturn_events();
    // Mid-turn slice, but "now" is one ms past the window from the last event.
    let last_ts = BASE_TS + (MID_TURN_INDEX as i64) * TS_STEP;
    let now = last_ts + DEFAULT_LIVE_WINDOW_MS + 1;
    assert!(!live_through(&events, MID_TURN_INDEX + 1, now));
}

#[test]
fn new_event_after_stop_returns_to_live() {
    let events = multiturn_events();
    // At the first Stop: idle. One event later (turn 2 resumes): live again.
    let stop_ts = BASE_TS + (FIRST_STOP_INDEX as i64) * TS_STEP;
    assert!(!live_through(
        &events,
        FIRST_STOP_INDEX + 1,
        stop_ts + TS_STEP
    ));

    let next_ts = BASE_TS + ((FIRST_STOP_INDEX + 1) as i64) * TS_STEP;
    assert!(live_through(
        &events,
        FIRST_STOP_INDEX + 2,
        next_ts + TS_STEP
    ));
}
