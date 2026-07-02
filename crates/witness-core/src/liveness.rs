//! First-class session liveness (issue #19), promoted from the provisional rule
//! that lived in [`crate::selector`].
//!
//! A session is *live* when both hold:
//! 1. its **last** observed event is not a `Stop` (it is mid-turn), and
//! 2. that last observed activity is within a recency window.
//!
//! Leniency on session start (issue #26): the rule no longer *requires* an
//! observed `SessionStart`. `init` before #26 registered only PreToolUse /
//! PostToolUse / Stop, so real sessions recorded by older configs never emit a
//! `SessionStart` — gating on it made every such session read idle forever. Any
//! observed event already proves the session started, so the first observed
//! event is treated as the start. This is strictly more tolerant of real data
//! and stays honest: we key the verdict on activity we actually saw, never on an
//! event we assume must have happened.
//!
//! Stop is per-turn, not per-session (issue #23): Claude Code's `Stop` hook
//! fires at the end of **every** assistant turn, not at session end. There is no
//! `SessionEnd` hook. So a live interactive session accumulates many `Stop`
//! events with more events after each one; treating "any observed `Stop`" as
//! terminal (the pre-#23 rule) made every interactive session read idle after
//! its first turn. The verdict therefore keys on whether the *last* event is a
//! `Stop` (between turns → idle) rather than on whether one was ever seen:
//! - last event is not `Stop`, within window → **live** (mid-turn)
//! - last event is `Stop` → **idle** (between turns; a later event returns it to
//!   live)
//! - last event beyond the window → **idle/stale** (as before)
//!
//! Determinism (a Core Value): the window is configurable (default
//! [`DEFAULT_LIVE_WINDOW_MS`]) and "now" is injected, never read from the
//! wall-clock, so a verdict is a pure function of recorded data and reproducible
//! in tests.
//!
//! Honesty (ADR-0002): liveness is *inferred* from a store scan, not proven. A
//! crashed agent whose last event was not a `Stop` reads as live until its
//! window lapses, then flips to idle — documented, not papered over. The rule
//! requires no daemon: it is derivable from the JSONL store alone (issue #19).

/// Default liveness window in milliseconds: a started, unstopped session whose
/// last event is within this span of "now" is treated as live (5 minutes).
pub const DEFAULT_LIVE_WINDOW_MS: i64 = 5 * 60 * 1000;

/// The minimal facts a liveness verdict needs, decoupled from any richer
/// summary so the rule stays a tiny, exhaustively testable pure function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LivenessInputs {
    /// The session's **last** observed event is a `Stop` (i.e. it is currently
    /// between turns). Not "a `Stop` was ever seen": `Stop` fires per turn, so a
    /// live session has many, each followed by more events (issue #23).
    pub last_is_stop: bool,
    /// Time of the last observed event, if any. `None` means no events were
    /// observed at all, which reads as not live (nothing to be recent).
    pub last_event_ts: Option<i64>,
}

/// Decide whether a session is live given its facts, the current time, and the
/// recency `window_ms`. Pure and total (`saturating_sub` guards a clock that
/// appears to move backwards).
///
/// Does not require an observed `SessionStart` (issue #26): any observed event
/// implies the session started, so an unstopped, recent last event is enough.
pub fn is_live(inputs: LivenessInputs, now_ms: i64, window_ms: i64) -> bool {
    if inputs.last_is_stop {
        return false;
    }
    match inputs.last_event_ts {
        Some(ts) => now_ms.saturating_sub(ts) <= window_ms,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 10_000_000;

    fn inputs(last_is_stop: bool, last: Option<i64>) -> LivenessInputs {
        LivenessInputs {
            last_is_stop,
            last_event_ts: last,
        }
    }

    #[test]
    fn live_when_mid_turn_and_recent() {
        // Last event is not a Stop (mid-turn), 1s ago, inside the window.
        let i = inputs(false, Some(NOW - 1_000));
        assert!(is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn between_turns_is_idle_even_if_recent() {
        // Last event IS a Stop: the turn just ended, so idle even though recent.
        let i = inputs(true, Some(NOW));
        assert!(!is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn new_event_after_stop_returns_to_live() {
        // Issue #23: a Stop is per-turn, not terminal. Once the next turn's event
        // arrives the last event is no longer a Stop, so the session is live again.
        let between_turns = inputs(true, Some(NOW - 1_000));
        assert!(!is_live(between_turns, NOW, DEFAULT_LIVE_WINDOW_MS));
        let next_turn_started = inputs(false, Some(NOW - 1_000));
        assert!(is_live(next_turn_started, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn stale_session_past_the_window_is_not_live() {
        // One millisecond beyond the window.
        let i = inputs(false, Some(NOW - DEFAULT_LIVE_WINDOW_MS - 1));
        assert!(!is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn live_without_observed_session_start() {
        // Issue #26: sessions recorded before init registered SessionStart have
        // no start event. A recent, unstopped last event still reads as live —
        // the first observed event is treated as the start.
        let i = inputs(false, Some(NOW - 1_000));
        assert!(is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn no_events_is_not_live() {
        let i = inputs(false, None);
        assert!(!is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn boundary_exactly_at_window_is_live() {
        // Exactly on the edge counts as live (inclusive window).
        let i = inputs(false, Some(NOW - DEFAULT_LIVE_WINDOW_MS));
        assert!(is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn window_is_configurable() {
        let i = inputs(false, Some(NOW - 10_000));
        // 5s window: 10s-old activity is idle.
        assert!(!is_live(i, NOW, 5_000));
        // 30s window: the same activity is live.
        assert!(is_live(i, NOW, 30_000));
    }

    #[test]
    fn clock_moving_backwards_does_not_panic_or_falsely_live() {
        // now < last_event_ts: saturating_sub yields 0, which is within window.
        let i = inputs(false, Some(NOW + 1_000));
        assert!(is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }
}
