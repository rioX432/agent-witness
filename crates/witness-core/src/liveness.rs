//! Session activity is inferred from recorded hook signals and an injected time.
//! Waiting persists until a later activity event; running falls back to idle
//! after the recency window. Transcript supplements cannot resolve a wait.

use crate::{AgentEvent, Attribution, EventKind, Source};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityState {
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "waiting:permission")]
    WaitingPermission,
    #[serde(rename = "waiting:input")]
    WaitingInput,
    #[serde(rename = "idle")]
    Idle,
}

impl ActivityState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::WaitingPermission => "waiting:permission",
            Self::WaitingInput => "waiting:input",
            Self::Idle => "idle",
        }
    }

    pub fn is_active(self) -> bool {
        self != Self::Idle
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityInputs {
    pub state: ActivityState,
    pub since_ms: Option<i64>,
    pub last_activity_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Activity {
    pub state: ActivityState,
    pub since_ms: Option<i64>,
    pub age_ms: Option<i64>,
    pub attribution: Attribution,
}

/// The state one event moves a session to, or `None` when the event says
/// nothing about activity (an `Error`, or a `Notification` that is not a wait).
/// Permission requests are not paired with decisions, so any other event
/// (including a parallel tool's result) counts as resumed activity.
pub fn event_activity(event: &AgentEvent) -> Option<ActivityState> {
    Some(match event.kind {
        EventKind::PermissionRequest => ActivityState::WaitingPermission,
        EventKind::Notification => match event
            .payload
            .get("notification_type")
            .and_then(|v| v.as_str())
        {
            Some("permission_prompt") => ActivityState::WaitingPermission,
            Some(
                "idle_prompt"
                | "agent_needs_input"
                | "elicitation_dialog"
                | "elicitation_url_dialog",
            ) => ActivityState::WaitingInput,
            // Unrelated notifications do not establish resumed activity.
            _ => return None,
        },
        EventKind::Stop | EventKind::SessionEnd | EventKind::Interrupt => ActivityState::Idle,
        EventKind::Error => return None,
        _ => ActivityState::Running,
    })
}

/// Derive transitions in append order, without pairing permission requests.
pub fn activity_inputs(events: &[AgentEvent]) -> ActivityInputs {
    let has_hooks = events.iter().any(|event| event.source == Source::Hooks);
    let mut inputs = ActivityInputs {
        state: ActivityState::Idle,
        since_ms: None,
        last_activity_ms: None,
    };
    for event in events
        .iter()
        .filter(|event| !has_hooks || event.source == Source::Hooks)
    {
        let Some(next) = event_activity(event) else {
            continue;
        };
        // A repeated state keeps its original start so the age counts the whole wait.
        if next != inputs.state || inputs.since_ms.is_none() {
            inputs.since_ms = Some(event.ts);
        }
        inputs.state = next;
        inputs.last_activity_ms = Some(event.ts);
    }
    inputs
}

/// Current activity is inferred, including whether a direct wait signal still applies.
pub fn infer_activity(inputs: ActivityInputs, now_ms: i64, window_ms: i64) -> Activity {
    let stale = inputs.state == ActivityState::Running
        && inputs
            .last_activity_ms
            .is_some_and(|ts| now_ms.saturating_sub(ts) > window_ms);
    let state = if stale {
        ActivityState::Idle
    } else {
        inputs.state
    };
    let since_ms = if stale {
        inputs
            .last_activity_ms
            .map(|ts| ts.saturating_add(window_ms))
    } else {
        inputs.since_ms
    };
    Activity {
        state,
        since_ms,
        age_ms: since_ms.map(|ts| now_ms.saturating_sub(ts).max(0)),
        attribution: Attribution::Inferred,
    }
}

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
    /// Derived activity when available; None retains the legacy recency rule.
    pub state: Option<ActivityState>,
    /// The session's **last** observed event is a `SessionEnd`: termination was
    /// directly observed, so the session is idle with no window wait. Last-event
    /// only, because a resumed session appends events afterwards (issue #31).
    pub last_is_session_end: bool,
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
    if inputs.state == Some(ActivityState::Idle)
        || inputs.last_is_stop
        || inputs.last_is_session_end
    {
        return false;
    }
    if matches!(
        inputs.state,
        Some(ActivityState::WaitingPermission | ActivityState::WaitingInput)
    ) {
        return true;
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
            state: None,
            last_is_session_end: false,
            last_event_ts: last,
        }
    }

    fn ended_inputs(last: Option<i64>) -> LivenessInputs {
        LivenessInputs {
            last_is_stop: false,
            state: None,
            last_is_session_end: true,
            last_event_ts: last,
        }
    }

    fn signal(ts: i64, kind: EventKind, notification_type: Option<&str>) -> AgentEvent {
        AgentEvent::new(
            ts,
            "waiting",
            Source::Hooks,
            kind,
            Attribution::Direct,
            crate::CONFIDENCE_CERTAIN,
            serde_json::json!({"notification_type": notification_type}),
        )
    }

    #[test]
    fn waiting_signals_resume_on_the_next_tool_call_with_injected_clock() {
        use crate::{Clock, FixedClock};
        const STEP_MS: i64 = 1_000;
        for (kind, notification_type, expected) in [
            (
                EventKind::PermissionRequest,
                None,
                ActivityState::WaitingPermission,
            ),
            (
                EventKind::Notification,
                Some("permission_prompt"),
                ActivityState::WaitingPermission,
            ),
            (
                EventKind::Notification,
                Some("idle_prompt"),
                ActivityState::WaitingInput,
            ),
            (
                EventKind::Notification,
                Some("agent_needs_input"),
                ActivityState::WaitingInput,
            ),
        ] {
            let clock = FixedClock(NOW + STEP_MS);
            let mut events = vec![signal(NOW, kind, notification_type)];
            let waiting = infer_activity(
                activity_inputs(&events),
                clock.now_ms(),
                DEFAULT_LIVE_WINDOW_MS,
            );
            assert_eq!(waiting.state, expected);
            assert_eq!(waiting.since_ms, Some(NOW));
            assert_eq!(waiting.age_ms, Some(STEP_MS));
            assert_eq!(waiting.attribution, Attribution::Inferred);
            assert_eq!(
                infer_activity(
                    activity_inputs(&events),
                    NOW + DEFAULT_LIVE_WINDOW_MS + STEP_MS,
                    DEFAULT_LIVE_WINDOW_MS
                )
                .state,
                expected
            );
            events.push(signal(clock.now_ms(), EventKind::ToolCall, None));
            let running = infer_activity(
                activity_inputs(&events),
                clock.now_ms(),
                DEFAULT_LIVE_WINDOW_MS,
            );
            assert_eq!(running.state, ActivityState::Running);
            assert_eq!(running.since_ms, Some(clock.now_ms()));
            assert_eq!(running.age_ms, Some(0));
        }
    }

    #[test]
    fn repeated_wait_preserves_start_and_transcripts_do_not_resolve_it() {
        let mut transcript = signal(NOW + 2, EventKind::Prompt, None);
        transcript.source = Source::Transcript;
        let events = vec![
            signal(NOW, EventKind::PermissionRequest, None),
            signal(NOW + 1, EventKind::Notification, Some("permission_prompt")),
            transcript,
        ];
        let activity = infer_activity(activity_inputs(&events), NOW + 3, DEFAULT_LIVE_WINDOW_MS);
        assert_eq!(activity.state, ActivityState::WaitingPermission);
        assert_eq!(activity.since_ms, Some(NOW));
    }

    #[test]
    fn turn_end_interrupt_and_denial_clear_wait_without_claiming_a_human_decision() {
        for (kind, expected) in [
            (EventKind::Stop, ActivityState::Idle),
            (EventKind::SessionEnd, ActivityState::Idle),
            (EventKind::Interrupt, ActivityState::Idle),
            (EventKind::PermissionDenied, ActivityState::Running),
            (EventKind::Prompt, ActivityState::Running),
        ] {
            let events = vec![
                signal(NOW, EventKind::PermissionRequest, None),
                signal(NOW + 1, kind, None),
            ];
            let activity =
                infer_activity(activity_inputs(&events), NOW + 2, DEFAULT_LIVE_WINDOW_MS);
            assert_eq!(activity.state, expected);
            assert_eq!(activity.since_ms, Some(NOW + 1));
            assert_eq!(activity.attribution, Attribution::Inferred);
        }
    }

    #[test]
    fn running_age_tracks_transition_and_staleness_has_its_own_start() {
        let events = vec![
            signal(NOW, EventKind::ToolCall, None),
            signal(NOW + 1, EventKind::ToolResult, None),
        ];
        let inputs = activity_inputs(&events);
        assert_eq!(
            infer_activity(inputs, NOW + 2, DEFAULT_LIVE_WINDOW_MS).since_ms,
            Some(NOW)
        );
        let idle = infer_activity(
            inputs,
            NOW + DEFAULT_LIVE_WINDOW_MS + 2,
            DEFAULT_LIVE_WINDOW_MS,
        );
        assert_eq!(idle.state, ActivityState::Idle);
        assert_eq!(idle.since_ms, Some(NOW + 1 + DEFAULT_LIVE_WINDOW_MS));
        assert_eq!(
            infer_activity(inputs, NOW - 1, DEFAULT_LIVE_WINDOW_MS).age_ms,
            Some(0)
        );
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
    fn session_end_is_idle_immediately_even_within_window() {
        // Issue #31: a directly observed SessionEnd needs no recency-window
        // wait — the session ended one millisecond ago and is already idle.
        let i = ended_inputs(Some(NOW - 1));
        assert!(!is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn new_event_after_session_end_returns_to_live() {
        // A resumed session appends events after its SessionEnd; the last event
        // is then no longer a SessionEnd, so the session reads live again.
        let ended = ended_inputs(Some(NOW - 1_000));
        assert!(!is_live(ended, NOW, DEFAULT_LIVE_WINDOW_MS));
        let resumed = inputs(false, Some(NOW - 1_000));
        assert!(is_live(resumed, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn clock_moving_backwards_does_not_panic_or_falsely_live() {
        // now < last_event_ts: saturating_sub yields 0, which is within window.
        let i = inputs(false, Some(NOW + 1_000));
        assert!(is_live(i, NOW, DEFAULT_LIVE_WINDOW_MS));
    }

    #[test]
    fn elicitation_notifications_map_to_waiting_input() {
        for kind in ["elicitation_dialog", "elicitation_url_dialog"] {
            let event = signal(1, EventKind::Notification, Some(kind));
            assert_eq!(
                event_activity(&event),
                Some(ActivityState::WaitingInput),
                "{kind}"
            );
        }
    }

    #[test]
    fn unrelated_notification_and_error_do_not_move_state_or_start() {
        const WAIT_TS: i64 = 100;
        for ignored in [
            signal(200, EventKind::Notification, Some("auth_success")),
            signal(201, EventKind::Notification, None),
            signal(202, EventKind::Error, None),
        ] {
            assert_eq!(event_activity(&ignored), None);
            let events = [signal(WAIT_TS, EventKind::PermissionRequest, None), ignored];
            let inputs = activity_inputs(&events);
            assert_eq!(inputs.state, ActivityState::WaitingPermission);
            assert_eq!(inputs.since_ms, Some(WAIT_TS));
            assert_eq!(inputs.last_activity_ms, Some(WAIT_TS));
        }
    }

    #[test]
    fn no_events_is_idle_with_no_start() {
        let inputs = activity_inputs(&[]);
        assert_eq!(inputs.state, ActivityState::Idle);
        assert_eq!(inputs.since_ms, None);
        assert_eq!(inputs.last_activity_ms, None);
    }
}
