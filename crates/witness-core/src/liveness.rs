//! First-class session liveness (issue #19), promoted from the provisional rule
//! that lived in [`crate::selector`].
//!
//! A session is *live* when all three hold:
//! 1. we have seen it start (a `SessionStart` event),
//! 2. we have **not** seen it stop (no `Stop` event), and
//! 3. its last observed activity is within a recency window.
//!
//! Determinism (a Core Value): the window is configurable (default
//! [`DEFAULT_LIVE_WINDOW_MS`]) and "now" is injected, never read from the
//! wall-clock, so a verdict is a pure function of recorded data and reproducible
//! in tests.
//!
//! Honesty (ADR-0002): liveness is *inferred* from a store scan, not proven. A
//! crashed agent that never emitted `Stop` reads as live until its window
//! lapses, then flips to idle — documented, not papered over. The rule requires
//! no daemon: it is derivable from the JSONL store alone (issue #19).

/// Default liveness window in milliseconds: a started, unstopped session whose
/// last event is within this span of "now" is treated as live (5 minutes).
pub const DEFAULT_LIVE_WINDOW_MS: i64 = 5 * 60 * 1000;

/// The minimal facts a liveness verdict needs, decoupled from any richer
/// summary so the rule stays a tiny, exhaustively testable pure function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LivenessInputs {
    /// A `SessionStart` event was observed.
    pub has_start: bool,
    /// A `Stop` event was observed (terminal — no `SessionEnd` hook exists today).
    pub has_stop: bool,
    /// Time of the last observed event, if any.
    pub last_event_ts: Option<i64>,
}

/// Decide whether a session is live given its facts, the current time, and the
/// recency `window_ms`. Pure and total (`saturating_sub` guards a clock that
/// appears to move backwards).
pub fn is_live(inputs: LivenessInputs, now_ms: i64, window_ms: i64) -> bool {
    if !inputs.has_start || inputs.has_stop {
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

    fn inputs(has_start: bool, has_stop: bool, last: Option<i64>) -> LivenessInputs {
        LivenessInputs {
            has_start,
            has_stop,
            last_event_ts: last,
        }
    }

    #[test]
    fn live_when_started_unstopped_and_recent() {
        // Last activity 1s ago, well inside the default window.
        let i = inputs(true, false, Some(NOW - 1_000));
        assert!(is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn stopped_session_is_not_live_even_if_recent() {
        let i = inputs(true, true, Some(NOW));
        assert!(!is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn stale_session_past_the_window_is_not_live() {
        // One millisecond beyond the window.
        let i = inputs(true, false, Some(NOW - DEFAULT_LIVE_WINDOW_MS - 1));
        assert!(!is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn never_started_session_is_not_live() {
        let i = inputs(false, false, Some(NOW));
        assert!(!is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn no_events_is_not_live() {
        let i = inputs(true, false, None);
        assert!(!is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn boundary_exactly_at_window_is_live() {
        // Exactly on the edge counts as live (inclusive window).
        let i = inputs(true, false, Some(NOW - DEFAULT_LIVE_WINDOW_MS));
        assert!(is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn window_is_configurable() {
        let i = inputs(true, false, Some(NOW - 10_000));
        // 5s window: 10s-old activity is idle.
        assert!(!is_live(i, NOW, 5_000));
        // 30s window: the same activity is live.
        assert!(is_live(i, NOW, 30_000));
    }

    #[test]
    fn clock_moving_backwards_does_not_panic_or_falsely_live() {
        // now < last_event_ts: saturating_sub yields 0, which is within window.
        let i = inputs(true, false, Some(NOW + 1_000));
        assert!(is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }
}
