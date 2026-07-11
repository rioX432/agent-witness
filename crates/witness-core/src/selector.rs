//! Session selectors: resolve a user-supplied selector (or none) to a concrete
//! session id. Shared by `show`, `report`, and the future `top` so the whole CLI
//! agrees on what "the latest session" means.
//!
//! Determinism (a Core Value): resolution is a pure function of a slice of
//! [`SessionSummary`], the current directory, and an injected `now_ms`. It reads
//! no wall-clock and does no I/O, so recency and liveness decisions are testable
//! and reproducible. The only I/O lives in [`collect_summaries`], a thin wrapper
//! that reads the store and delegates every judgement to [`summarize`].
//!
//! Liveness note: the `@live` selector delegates to [`crate::liveness`] (issue
//! #19) — started, last event is not a `Stop` (mid-turn, not between turns —
//! `Stop` is per-turn, issue #23), and that event is within
//! [`DEFAULT_LIVE_WINDOW_MS`]. Honesty (ADR-0002): liveness is inferred, never
//! proven.

use crate::event::{AgentEvent, EventKind, Source};
use crate::liveness::{self, LivenessInputs, DEFAULT_LIVE_WINDOW_MS};
use crate::store::{SessionStore, StoreError};

/// Marker that a selector is a scope/relative selector rather than an id prefix.
const SELECTOR_AT: char = '@';
/// Selector for the single most recent session.
const SELECTOR_LAST: &str = "@last";
/// Prefix for the substring-on-cwd project selector.
const SELECTOR_PROJECT: &str = "@project:";
/// Prefix for the n-th live session selector.
const SELECTOR_LIVE: &str = "@live:";
/// Payload field carrying the working directory of a hook event.
const FIELD_CWD: &str = "cwd";
/// Smallest accepted ordinal for `@N` / `@live:n` (1-based, git-style).
const FIRST_ORDINAL: usize = 1;

/// A compact, judgement-ready summary of one recorded session.
///
/// Built once per session by [`summarize`] (pure) or [`collect_summaries`]
/// (reads the store). Carries exactly what selector resolution and the pick
/// view need, so resolution never has to re-read event logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// Session id (store directory name).
    pub id: String,
    /// Recorded session start (`meta.created_ts`), if available.
    pub created_ts: Option<i64>,
    /// Time of the first event, if any.
    pub first_event_ts: Option<i64>,
    /// Time of the last event, if any.
    pub last_event_ts: Option<i64>,
    /// Distinct working directories observed on the session's events, in
    /// first-seen order.
    pub cwds: Vec<String>,
    /// Whether a `SessionStart` event was observed. Informational only: the
    /// liveness rule no longer gates on it (issue #26 — sessions recorded by
    /// pre-#26 configs never emit one), but it still records honestly whether we
    /// saw the session's own start.
    pub has_start: bool,
    /// Whether the last **hook-observed** event is a `Stop`, i.e. the session
    /// is currently between turns (used by the liveness rule). `Stop` is
    /// per-turn, not terminal, so this tracks the last hook event's kind — not
    /// whether any `Stop` was ever seen (issue #23). Transcript-sourced
    /// supplements are ignored here: they are ingested right after the hook
    /// record that triggered them and must not mask it.
    pub last_is_stop: bool,
    /// Whether the last **hook-observed** event is a `SessionEnd`: termination
    /// was directly observed (issue #31). A resumed session appends hook
    /// events after its `SessionEnd` and reads live again.
    pub last_is_session_end: bool,
    /// Total parsed events.
    pub event_count: usize,
    /// Number of tool invocations (`ToolCall` events).
    pub tool_calls: usize,
}

impl SessionSummary {
    /// Best available start time: the recorded meta start, else the first event.
    pub fn started_ms(&self) -> Option<i64> {
        self.created_ts.or(self.first_event_ts)
    }

    /// Recency key for ordering: the last event's time, else the recorded start.
    /// Absent both, `0` (an empty session sorts oldest).
    pub fn recency_ms(&self) -> i64 {
        self.last_event_ts.or(self.created_ts).unwrap_or(0)
    }

    /// The facts the [`crate::liveness`] rule needs from this summary.
    pub fn liveness_inputs(&self) -> LivenessInputs {
        LivenessInputs {
            last_is_stop: self.last_is_stop,
            last_is_session_end: self.last_is_session_end,
            last_event_ts: self.last_event_ts,
        }
    }

    /// Liveness under the default window ([`DEFAULT_LIVE_WINDOW_MS`]).
    pub fn is_live(&self, now_ms: i64) -> bool {
        self.is_live_within(now_ms, DEFAULT_LIVE_WINDOW_MS)
    }

    /// Liveness under a caller-supplied recency window (`ls --live`, `top`).
    pub fn is_live_within(&self, now_ms: i64, window_ms: i64) -> bool {
        liveness::is_live(self.liveness_inputs(), now_ms, window_ms)
    }
}

/// The outcome of resolving a selector: the chosen session and whether the
/// no-argument default had to fall back from the current directory to the global
/// latest (the caller surfaces a one-line note when this is set — ADR-0002).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// The resolved session id.
    pub session_id: String,
    /// `true` when the no-arg default found no cwd-matching session and fell back
    /// to the globally most recent one.
    pub cwd_fallback: bool,
}

/// Why a selector could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectorError {
    /// No sessions are recorded at all.
    #[error("no sessions recorded yet")]
    NoSessions,
    /// A well-formed selector matched no session.
    #[error("no session matches selector `{selector}`")]
    NoMatch {
        /// The selector as supplied.
        selector: String,
    },
    /// An id prefix matched more than one session.
    #[error("ambiguous session prefix `{prefix}`; candidates: {}", candidates.join(", "))]
    AmbiguousPrefix {
        /// The prefix as supplied.
        prefix: String,
        /// Matching session ids, sorted.
        candidates: Vec<String>,
    },
    /// A relative/live ordinal pointed past the available sessions.
    #[error("selector `{selector}` is out of range ({available} session(s) available)")]
    OutOfRange {
        /// The selector as supplied.
        selector: String,
        /// How many sessions were available to index into.
        available: usize,
    },
    /// The selector syntax was not recognized.
    #[error("invalid selector `{0}`")]
    InvalidSelector(String),
}

/// Summarize one session from its metadata start and its parsed events. Pure: no
/// I/O, no wall-clock — every field derives from the inputs.
pub fn summarize(id: &str, created_ts: Option<i64>, events: &[AgentEvent]) -> SessionSummary {
    let mut cwds: Vec<String> = Vec::new();
    let mut has_start = false;
    let mut tool_calls = 0;
    for ev in events {
        if let Some(cwd) = ev.payload.get(FIELD_CWD).and_then(|v| v.as_str()) {
            if !cwds.iter().any(|c| c == cwd) {
                cwds.push(cwd.to_string());
            }
        }
        match ev.kind {
            EventKind::SessionStart => has_start = true,
            EventKind::ToolCall => tool_calls += 1,
            _ => {}
        }
    }
    // `Stop` is per-turn, not terminal (issue #23); `SessionEnd` is terminal
    // but a resumed session appends events after it (issue #31). Both key on
    // the last HOOK-observed event: transcript supplements are ingested right
    // after the `Stop`/`SessionEnd` record that triggered them, so keying on
    // the raw last line would let an `observed` transcript event mask a
    // directly observed turn end / session end.
    let last_hook_kind = events
        .iter()
        .rev()
        .find(|e| e.source == Source::Hooks)
        .map(|e| e.kind);
    let last_is_stop = last_hook_kind == Some(EventKind::Stop);
    let last_is_session_end = last_hook_kind == Some(EventKind::SessionEnd);
    SessionSummary {
        id: id.to_string(),
        created_ts,
        first_event_ts: events.first().map(|e| e.ts),
        last_event_ts: events.last().map(|e| e.ts),
        cwds,
        has_start,
        last_is_stop,
        last_is_session_end,
        event_count: events.len(),
        tool_calls,
    }
}

/// Read every session from the store and build its [`SessionSummary`].
///
/// I/O boundary only: it reads meta and events, then hands off to [`summarize`]
/// for all judgement. Returns summaries in the store's id order.
pub fn collect_summaries(store: &SessionStore) -> Result<Vec<SessionSummary>, StoreError> {
    let mut summaries = Vec::new();
    for id in store.list_sessions()? {
        let read = store.read(&id)?;
        let created_ts = store.read_meta(&id).ok().map(|m| m.created_ts);
        summaries.push(summarize(&id, created_ts, &read.events));
    }
    Ok(summaries)
}

/// Resolve a selector (or `None` for the no-argument default) to a session id.
///
/// - `None`: the most recent session belonging to `cwd`; if none, the globally
///   most recent, with [`Resolution::cwd_fallback`] set.
/// - `@last`: the single most recent session.
/// - `@N` (N ≥ 1): the N-th most recent session (`@1` == `@last`).
/// - `@project:<substring>`: the most recent session any of whose cwds contains
///   `<substring>`.
/// - `@live:<n>`: the n-th most recent live session ([`crate::liveness`]).
/// - otherwise: a unique session-id prefix (ambiguity lists the candidates).
pub fn resolve(
    summaries: &[SessionSummary],
    selector: Option<&str>,
    cwd: &str,
    now_ms: i64,
) -> Result<Resolution, SelectorError> {
    if summaries.is_empty() {
        return Err(SelectorError::NoSessions);
    }
    match selector {
        None => Ok(resolve_default(summaries, cwd)),
        Some(s) if s.starts_with(SELECTOR_AT) => resolve_at(summaries, s, now_ms),
        Some(s) => resolve_prefix(summaries, s),
    }
}

/// Summaries ordered most-recent-first; ties broken by id for a stable order.
fn by_recency_desc(summaries: &[SessionSummary]) -> Vec<&SessionSummary> {
    let mut ordered: Vec<&SessionSummary> = summaries.iter().collect();
    ordered.sort_by(|a, b| {
        b.recency_ms()
            .cmp(&a.recency_ms())
            .then_with(|| a.id.cmp(&b.id))
    });
    ordered
}

/// No-argument default: latest session belonging to `cwd`, else global latest.
fn resolve_default(summaries: &[SessionSummary], cwd: &str) -> Resolution {
    let ordered = by_recency_desc(summaries);
    if let Some(matched) = ordered
        .iter()
        .find(|s| s.cwds.iter().any(|c| path_belongs(cwd, c)))
    {
        return Resolution {
            session_id: matched.id.clone(),
            cwd_fallback: false,
        };
    }
    // Non-empty by the caller's guard, so `ordered[0]` always exists.
    Resolution {
        session_id: ordered[0].id.clone(),
        cwd_fallback: true,
    }
}

/// Resolve an `@`-prefixed selector.
fn resolve_at(
    summaries: &[SessionSummary],
    selector: &str,
    now_ms: i64,
) -> Result<Resolution, SelectorError> {
    if selector == SELECTOR_LAST {
        return Ok(ok(by_recency_desc(summaries)[0]));
    }
    if let Some(sub) = selector.strip_prefix(SELECTOR_PROJECT) {
        return resolve_project(summaries, selector, sub);
    }
    if let Some(n) = selector.strip_prefix(SELECTOR_LIVE) {
        return resolve_live(summaries, selector, n, now_ms);
    }
    // `@N`: an ordinal into the recency-ordered list.
    let ordinal = selector.trim_start_matches(SELECTOR_AT);
    match parse_ordinal(ordinal) {
        Some(n) => nth_recent(summaries, selector, n),
        None => Err(SelectorError::InvalidSelector(selector.to_string())),
    }
}

/// `@project:<substring>`: latest session any of whose cwds contains `sub`.
fn resolve_project(
    summaries: &[SessionSummary],
    selector: &str,
    sub: &str,
) -> Result<Resolution, SelectorError> {
    if sub.is_empty() {
        return Err(SelectorError::InvalidSelector(selector.to_string()));
    }
    by_recency_desc(summaries)
        .into_iter()
        .find(|s| s.cwds.iter().any(|c| c.contains(sub)))
        .map(ok)
        .ok_or_else(|| SelectorError::NoMatch {
            selector: selector.to_string(),
        })
}

/// `@live:<n>`: the n-th most recent live session ([`crate::liveness`]).
fn resolve_live(
    summaries: &[SessionSummary],
    selector: &str,
    n: &str,
    now_ms: i64,
) -> Result<Resolution, SelectorError> {
    let Some(index) = parse_ordinal(n) else {
        return Err(SelectorError::InvalidSelector(selector.to_string()));
    };
    let live: Vec<&SessionSummary> = by_recency_desc(summaries)
        .into_iter()
        .filter(|s| s.is_live(now_ms))
        .collect();
    match live.get(index - FIRST_ORDINAL) {
        Some(s) => Ok(ok(s)),
        None if live.is_empty() => Err(SelectorError::NoMatch {
            selector: selector.to_string(),
        }),
        None => Err(SelectorError::OutOfRange {
            selector: selector.to_string(),
            available: live.len(),
        }),
    }
}

/// The n-th most recent session (1-based) over all sessions.
fn nth_recent(
    summaries: &[SessionSummary],
    selector: &str,
    n: usize,
) -> Result<Resolution, SelectorError> {
    let ordered = by_recency_desc(summaries);
    ordered
        .get(n - FIRST_ORDINAL)
        .map(|s| ok(s))
        .ok_or_else(|| SelectorError::OutOfRange {
            selector: selector.to_string(),
            available: ordered.len(),
        })
}

/// Resolve a session-id prefix: an exact id, else a unique prefix.
fn resolve_prefix(summaries: &[SessionSummary], prefix: &str) -> Result<Resolution, SelectorError> {
    if let Some(exact) = summaries.iter().find(|s| s.id == prefix) {
        return Ok(ok(exact));
    }
    let mut candidates: Vec<String> = summaries
        .iter()
        .filter(|s| s.id.starts_with(prefix))
        .map(|s| s.id.clone())
        .collect();
    match candidates.len() {
        0 => Err(SelectorError::NoMatch {
            selector: prefix.to_string(),
        }),
        1 => Ok(Resolution {
            session_id: candidates.remove(0),
            cwd_fallback: false,
        }),
        _ => {
            candidates.sort();
            Err(SelectorError::AmbiguousPrefix {
                prefix: prefix.to_string(),
                candidates,
            })
        }
    }
}

/// Build a non-fallback resolution for a chosen summary.
fn ok(summary: &SessionSummary) -> Resolution {
    Resolution {
        session_id: summary.id.clone(),
        cwd_fallback: false,
    }
}

/// Parse a 1-based ordinal (`"1"`, `"2"`, …), rejecting empty, non-digit, and 0.
fn parse_ordinal(s: &str) -> Option<usize> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<usize>().ok().filter(|&n| n >= FIRST_ORDINAL)
}

/// Whether a session recorded at `session_cwd` belongs to the project at
/// `current`: the same directory, or one nested within the other. Comparison is
/// path-component aware so `/a/proj` never matches `/a/project` by accident.
fn path_belongs(current: &str, session_cwd: &str) -> bool {
    current == session_cwd
        || is_descendant(current, session_cwd)
        || is_descendant(session_cwd, current)
}

/// Whether `child` is strictly nested under `ancestor` (component-aware).
fn is_descendant(child: &str, ancestor: &str) -> bool {
    // The filesystem root already ends in the separator, so any longer
    // absolute path is nested under it.
    if ancestor == "/" {
        return child.len() > 1 && child.starts_with('/');
    }
    match child.strip_prefix(ancestor) {
        Some(rest) => rest.starts_with('/'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Attribution, Source, CONFIDENCE_CERTAIN};
    use serde_json::json;

    const NOW: i64 = 10_000_000;

    fn event(ts: i64, kind: EventKind, cwd: Option<&str>) -> AgentEvent {
        let payload = match cwd {
            Some(c) => json!({ "cwd": c, "tool_name": "Read" }),
            None => json!({ "tool_name": "Read" }),
        };
        AgentEvent::new(
            ts,
            "s",
            Source::Hooks,
            kind,
            Attribution::Direct,
            CONFIDENCE_CERTAIN,
            payload,
        )
    }

    /// A started (but unstopped) summary with a single cwd and `last_event_ts`
    /// = recency; its recent, non-`Stop` last event makes it a live candidate.
    fn summary(id: &str, recency: i64, cwd: &str) -> SessionSummary {
        summarize(
            id,
            Some(recency),
            &[
                event(recency - 2, EventKind::SessionStart, Some(cwd)),
                event(recency - 1, EventKind::ToolCall, Some(cwd)),
                event(recency, EventKind::ToolResult, Some(cwd)),
            ],
        )
    }

    #[test]
    fn summarize_extracts_cwds_stop_and_counts() {
        let events = vec![
            event(1, EventKind::SessionStart, Some("/a/proj")),
            event(2, EventKind::ToolCall, Some("/a/proj")),
            event(3, EventKind::ToolResult, Some("/a/proj")),
            event(4, EventKind::Stop, Some("/a/proj")),
        ];
        let s = summarize("sess", Some(0), &events);
        assert_eq!(s.cwds, vec!["/a/proj"]);
        assert!(s.has_start);
        // The last event here is the Stop, so the session reads as between-turns.
        assert!(s.last_is_stop);
        assert_eq!(s.tool_calls, 1);
        assert_eq!(s.event_count, 4);
        assert_eq!(s.first_event_ts, Some(1));
        assert_eq!(s.last_event_ts, Some(4));
        assert_eq!(s.recency_ms(), 4);
        assert_eq!(s.started_ms(), Some(0));
    }

    #[test]
    fn summarize_keys_end_state_on_last_hook_event_not_transcript_supplements() {
        // Transcript ingest runs right after the Stop/SessionEnd record lands
        // (emit/watch), appending observed events after it in file order. Those
        // supplements must not mask the directly observed end state.
        let mut end = event(4, EventKind::SessionEnd, Some("/a/proj"));
        end.payload = json!({ "reason": "logout" });
        let transcript_supplement = AgentEvent::new(
            3,
            "s",
            Source::Transcript,
            EventKind::Prompt,
            Attribution::Observed,
            CONFIDENCE_CERTAIN,
            json!({ "text": "assistant prose" }),
        );
        let events = vec![
            event(1, EventKind::ToolCall, Some("/a/proj")),
            end,
            transcript_supplement,
        ];
        let s = summarize("sess", Some(0), &events);
        assert!(
            s.last_is_session_end,
            "a trailing transcript event must not mask SessionEnd"
        );
        assert!(!s.last_is_stop);

        // Same masking rule for the per-turn Stop.
        let events = vec![
            event(1, EventKind::ToolCall, Some("/a/proj")),
            event(4, EventKind::Stop, Some("/a/proj")),
            AgentEvent::new(
                3,
                "s",
                Source::Transcript,
                EventKind::Prompt,
                Attribution::Observed,
                CONFIDENCE_CERTAIN,
                json!({ "text": "assistant prose" }),
            ),
        ];
        let s = summarize("sess", Some(0), &events);
        assert!(
            s.last_is_stop,
            "a trailing transcript event must not mask Stop"
        );
    }

    #[test]
    fn empty_selection_errors_when_no_sessions() {
        assert_eq!(
            resolve(&[], None, "/x", NOW),
            Err(SelectorError::NoSessions)
        );
    }

    #[test]
    fn no_arg_default_prefers_current_cwd_latest() {
        let sessions = vec![
            summary("old-here", 100, "/home/me/proj"),
            summary("newest-elsewhere", 500, "/home/me/other"),
            summary("newer-here", 300, "/home/me/proj"),
        ];
        let r = resolve(&sessions, None, "/home/me/proj", NOW).unwrap();
        assert_eq!(r.session_id, "newer-here");
        assert!(!r.cwd_fallback);
    }

    #[test]
    fn no_arg_default_matches_nested_directory() {
        let sessions = vec![summary("s", 100, "/home/me/proj")];
        // Running from a subdirectory still belongs to the project.
        let r = resolve(&sessions, None, "/home/me/proj/crates/core", NOW).unwrap();
        assert_eq!(r.session_id, "s");
        assert!(!r.cwd_fallback);
    }

    #[test]
    fn no_arg_default_falls_back_to_global_latest_with_flag() {
        let sessions = vec![
            summary("a", 100, "/somewhere/else"),
            summary("b", 900, "/another/place"),
        ];
        let r = resolve(&sessions, None, "/home/me/proj", NOW).unwrap();
        assert_eq!(r.session_id, "b");
        assert!(r.cwd_fallback);
    }

    #[test]
    fn sibling_directory_is_not_a_prefix_match() {
        let sessions = vec![
            summary("proj", 100, "/a/proj"),
            summary("global-new", 200, "/z/z"),
        ];
        // `/a/project` must not match the session recorded at `/a/proj`.
        let r = resolve(&sessions, None, "/a/project", NOW).unwrap();
        assert_eq!(r.session_id, "global-new");
        assert!(r.cwd_fallback);
    }

    #[test]
    fn running_from_filesystem_root_matches_any_session() {
        // `/` nests every absolute path; the fallback note must not fire.
        let sessions = vec![summary("s", 100, "/home/me/proj")];
        let r = resolve(&sessions, None, "/", NOW).unwrap();
        assert_eq!(r.session_id, "s");
        assert!(!r.cwd_fallback);
    }

    #[test]
    fn at_last_and_ordinals_select_by_recency() {
        let sessions = vec![
            summary("first", 100, "/p"),
            summary("second", 200, "/p"),
            summary("third", 300, "/p"),
        ];
        assert_eq!(
            resolve(&sessions, Some("@last"), "/p", NOW)
                .unwrap()
                .session_id,
            "third"
        );
        assert_eq!(
            resolve(&sessions, Some("@1"), "/p", NOW)
                .unwrap()
                .session_id,
            "third"
        );
        assert_eq!(
            resolve(&sessions, Some("@2"), "/p", NOW)
                .unwrap()
                .session_id,
            "second"
        );
        assert_eq!(
            resolve(&sessions, Some("@3"), "/p", NOW)
                .unwrap()
                .session_id,
            "first"
        );
    }

    #[test]
    fn ordinal_out_of_range_reports_available() {
        let sessions = vec![summary("only", 1, "/p")];
        assert_eq!(
            resolve(&sessions, Some("@2"), "/p", NOW),
            Err(SelectorError::OutOfRange {
                selector: "@2".to_string(),
                available: 1,
            })
        );
    }

    #[test]
    fn project_selector_substring_matches_latest() {
        let sessions = vec![
            summary("old-avvy", 100, "/work/avvy-core"),
            summary("new-witness", 500, "/work/agent-witness"),
            summary("new-avvy", 300, "/work/avvy-app"),
        ];
        let r = resolve(&sessions, Some("@project:avvy"), "/x", NOW).unwrap();
        assert_eq!(r.session_id, "new-avvy");
    }

    #[test]
    fn project_selector_no_match_errors() {
        let sessions = vec![summary("s", 1, "/work/thing")];
        assert_eq!(
            resolve(&sessions, Some("@project:nope"), "/x", NOW),
            Err(SelectorError::NoMatch {
                selector: "@project:nope".to_string(),
            })
        );
    }

    #[test]
    fn empty_project_substring_is_invalid() {
        let sessions = vec![summary("s", 1, "/p")];
        assert_eq!(
            resolve(&sessions, Some("@project:"), "/x", NOW),
            Err(SelectorError::InvalidSelector("@project:".to_string()))
        );
    }

    #[test]
    fn live_selector_uses_liveness_rule() {
        // "stopped" has a Stop → not live; "stale" is beyond the window → not
        // live; "live-*" are within the window, started, with no Stop.
        let stopped = summarize(
            "stopped",
            Some(NOW),
            &[
                event(NOW - 1, EventKind::SessionStart, Some("/p")),
                event(NOW, EventKind::Stop, Some("/p")),
            ],
        );
        let stale = summary("stale", NOW - DEFAULT_LIVE_WINDOW_MS - 1, "/p");
        let live_older = summary("live-older", NOW - 1_000, "/p");
        let live_newer = summary("live-newer", NOW - 10, "/p");
        let sessions = vec![stopped, stale, live_older, live_newer];

        assert_eq!(
            resolve(&sessions, Some("@live:1"), "/p", NOW)
                .unwrap()
                .session_id,
            "live-newer"
        );
        assert_eq!(
            resolve(&sessions, Some("@live:2"), "/p", NOW)
                .unwrap()
                .session_id,
            "live-older"
        );
        assert_eq!(
            resolve(&sessions, Some("@live:3"), "/p", NOW),
            Err(SelectorError::OutOfRange {
                selector: "@live:3".to_string(),
                available: 2,
            })
        );
    }

    #[test]
    fn live_selector_no_live_sessions_errors() {
        let stopped = summarize(
            "stopped",
            Some(NOW),
            &[event(NOW, EventKind::Stop, Some("/p"))],
        );
        assert_eq!(
            resolve(&[stopped], Some("@live:1"), "/p", NOW),
            Err(SelectorError::NoMatch {
                selector: "@live:1".to_string(),
            })
        );
    }

    #[test]
    fn live_selector_includes_session_without_session_start() {
        // Issue #26: a session recorded by a pre-#26 config has no SessionStart,
        // yet its recent, unstopped last event must still read as live.
        let no_start = summarize(
            "no-start",
            Some(NOW),
            &[
                event(NOW - 1, EventKind::ToolCall, Some("/p")),
                event(NOW, EventKind::ToolResult, Some("/p")),
            ],
        );
        assert!(!no_start.has_start);
        assert_eq!(
            resolve(&[no_start], Some("@live:1"), "/p", NOW)
                .unwrap()
                .session_id,
            "no-start"
        );
    }

    #[test]
    fn prefix_exact_and_unique_match() {
        let sessions = vec![summary("abc123", 1, "/p"), summary("def456", 2, "/p")];
        // Exact.
        assert_eq!(
            resolve(&sessions, Some("abc123"), "/p", NOW)
                .unwrap()
                .session_id,
            "abc123"
        );
        // Unique prefix.
        assert_eq!(
            resolve(&sessions, Some("def"), "/p", NOW)
                .unwrap()
                .session_id,
            "def456"
        );
    }

    #[test]
    fn ambiguous_prefix_lists_sorted_candidates() {
        let sessions = vec![
            summary("sess-b", 1, "/p"),
            summary("sess-a", 2, "/p"),
            summary("other", 3, "/p"),
        ];
        assert_eq!(
            resolve(&sessions, Some("sess-"), "/p", NOW),
            Err(SelectorError::AmbiguousPrefix {
                prefix: "sess-".to_string(),
                candidates: vec!["sess-a".to_string(), "sess-b".to_string()],
            })
        );
    }

    #[test]
    fn unknown_prefix_is_no_match() {
        let sessions = vec![summary("real", 1, "/p")];
        assert_eq!(
            resolve(&sessions, Some("ghost"), "/p", NOW),
            Err(SelectorError::NoMatch {
                selector: "ghost".to_string(),
            })
        );
    }

    #[test]
    fn malformed_at_selectors_are_invalid() {
        let sessions = vec![summary("s", 1, "/p")];
        for bad in ["@", "@0", "@x", "@1x", "@live:x", "@live:0"] {
            assert!(
                matches!(
                    resolve(&sessions, Some(bad), "/p", NOW),
                    Err(SelectorError::InvalidSelector(_))
                ),
                "expected invalid selector for {bad:?}"
            );
        }
    }
}
