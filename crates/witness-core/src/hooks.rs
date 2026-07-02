//! Normalizer: Claude Code hook payloads → the canonical [`AgentEvent`] model.
//!
//! This is a pure mapping. It reads no wall-clock and does no I/O: the caller
//! supplies `ts` (inject a [`crate::Clock`]) and a `raw_ref` linking the event
//! to its verbatim raw record (ADR-0001). Hook-derived events are always
//! [`Attribution::Direct`] with full confidence (ADR-0002) — the agent reported
//! them itself.
//!
//! Failure representation: a `Bash` tool call that exits non-zero fires **no**
//! `PostToolUse` hook (empirically pinned by issue #8), so a failure shows up as
//! a [`EventKind::ToolCall`] with no matching [`EventKind::ToolResult`]. The
//! normalizer therefore never fabricates a failure from a single payload; it
//! only maps an explicit `PostToolUseFailure` hook to [`EventKind::ToolFailure`]
//! should upstream ever emit one.

use serde_json::{json, Map, Value};

use crate::event::{AgentEvent, Attribution, EventKind, Source, CONFIDENCE_CERTAIN};

/// Session id used when a payload cannot be attributed to a real session
/// (malformed JSON, missing `session_id`). Kept as a single safe path component
/// so the store accepts it.
pub const UNKNOWN_SESSION: &str = "unknown-session";

// Hook event names as sent by Claude Code (`hook_event_name`).
const HOOK_SESSION_START: &str = "SessionStart";
const HOOK_USER_PROMPT_SUBMIT: &str = "UserPromptSubmit";
const HOOK_PRE_TOOL_USE: &str = "PreToolUse";
const HOOK_POST_TOOL_USE: &str = "PostToolUse";
const HOOK_POST_TOOL_USE_FAILURE: &str = "PostToolUseFailure";
const HOOK_STOP: &str = "Stop";

/// Why a payload could not be normalized. The receiver turns these into an
/// [`EventKind::Error`] event rather than dropping the input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NormalizeError {
    /// The payload was valid JSON but not an object.
    #[error("hook payload is not a JSON object")]
    NotAnObject,
    /// A required string field was missing (e.g. `session_id`, `hook_event_name`).
    #[error("hook payload missing required field `{0}`")]
    MissingField(&'static str),
    /// The `hook_event_name` is not one this version maps.
    #[error("unmapped hook event `{0}`")]
    UnmappedHookEvent(String),
    /// The input on stdin / the socket was not valid JSON.
    #[error("malformed hook JSON: {0}")]
    MalformedJson(String),
    /// The payload carried a `session_id` that is not a safe path component.
    #[error("invalid session id `{0}`")]
    InvalidSessionId(String),
}

/// Field name carrying the session id on every hook payload.
pub const FIELD_SESSION_ID: &str = "session_id";
/// Field name carrying the hook event name on every hook payload.
pub const FIELD_HOOK_EVENT_NAME: &str = "hook_event_name";

/// Extract the session id from a (parsed) hook payload, if present and a string.
pub fn session_id_of(hook: &Value) -> Option<&str> {
    hook.get(FIELD_SESSION_ID).and_then(Value::as_str)
}

/// Normalize one parsed hook payload into an [`AgentEvent`].
///
/// `ts` is the receive time (caller-injected). `raw_ref` links the event to its
/// verbatim raw record. Returns [`NormalizeError`] for shapes the caller should
/// record as an error event instead.
pub fn normalize(hook: &Value, ts: i64, raw_ref: &str) -> Result<AgentEvent, NormalizeError> {
    let obj = hook.as_object().ok_or(NormalizeError::NotAnObject)?;
    let session = obj
        .get(FIELD_SESSION_ID)
        .and_then(Value::as_str)
        .ok_or(NormalizeError::MissingField(FIELD_SESSION_ID))?;
    let hook_event = obj
        .get(FIELD_HOOK_EVENT_NAME)
        .and_then(Value::as_str)
        .ok_or(NormalizeError::MissingField(FIELD_HOOK_EVENT_NAME))?;

    let (kind, payload) = match hook_event {
        HOOK_SESSION_START => (EventKind::SessionStart, project(obj, &["source", "cwd"])),
        HOOK_USER_PROMPT_SUBMIT => (
            EventKind::Prompt,
            project(obj, &["prompt", "prompt_id", "cwd"]),
        ),
        HOOK_PRE_TOOL_USE => (
            EventKind::ToolCall,
            project(obj, &["tool_name", "tool_use_id", "tool_input", "cwd"]),
        ),
        HOOK_POST_TOOL_USE => (
            EventKind::ToolResult,
            project(
                obj,
                &["tool_name", "tool_use_id", "tool_response", "duration_ms"],
            ),
        ),
        HOOK_POST_TOOL_USE_FAILURE => (
            EventKind::ToolFailure,
            project(
                obj,
                &["tool_name", "tool_use_id", "tool_response", "duration_ms"],
            ),
        ),
        HOOK_STOP => (
            EventKind::Stop,
            project(obj, &["last_assistant_message", "stop_hook_active"]),
        ),
        other => return Err(NormalizeError::UnmappedHookEvent(other.to_string())),
    };

    // Always carry the hook event name so a normalized event is self-describing.
    let mut payload = payload;
    if let Value::Object(map) = &mut payload {
        map.insert(FIELD_HOOK_EVENT_NAME.to_string(), json!(hook_event));
    }

    let mut event = AgentEvent::new(
        ts,
        session,
        Source::Hooks,
        kind,
        Attribution::Direct,
        CONFIDENCE_CERTAIN,
        payload,
    );
    event.raw_event_ref = Some(raw_ref.to_string());
    Ok(event)
}

/// Build an [`EventKind::Error`] event for a payload the recorder could not
/// normalize. Attribution is [`Attribution::Direct`]: we are certain we
/// received these bytes, even though we could not interpret them (ADR-0002).
pub fn error_event(session: &str, ts: i64, raw_ref: &str, reason: &NormalizeError) -> AgentEvent {
    let mut event = AgentEvent::new(
        ts,
        session,
        Source::Hooks,
        EventKind::Error,
        Attribution::Direct,
        CONFIDENCE_CERTAIN,
        json!({ "error": reason.to_string() }),
    );
    event.raw_event_ref = Some(raw_ref.to_string());
    event
}

/// Copy the listed keys (when present) from `obj` into a fresh JSON object,
/// preserving their values. Absent keys are simply skipped.
fn project(obj: &Map<String, Value>, keys: &[&str]) -> Value {
    let mut out = Map::new();
    for key in keys {
        if let Some(value) = obj.get(*key) {
            out.insert((*key).to_string(), value.clone());
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: i64 = 1_700_000_000_000;

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).expect("valid json")
    }

    #[test]
    fn pre_tool_use_maps_to_tool_call_with_direct_attribution() {
        let hook = parse(
            r#"{"hook_event_name":"PreToolUse","session_id":"s1","cwd":"/p",
                "tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"ls"}}"#,
        );
        let ev = normalize(&hook, TS, "raw-0").unwrap();
        assert_eq!(ev.kind, EventKind::ToolCall);
        assert_eq!(ev.attribution, Attribution::Direct);
        assert_eq!(ev.confidence, CONFIDENCE_CERTAIN);
        assert_eq!(ev.source, Source::Hooks);
        assert_eq!(ev.session, "s1");
        assert_eq!(ev.raw_event_ref.as_deref(), Some("raw-0"));
        assert_eq!(ev.payload["tool_name"], json!("Bash"));
        assert_eq!(ev.payload["tool_use_id"], json!("t1"));
        assert_eq!(ev.payload["tool_input"]["command"], json!("ls"));
        assert_eq!(ev.payload["hook_event_name"], json!("PreToolUse"));
    }

    #[test]
    fn post_tool_use_maps_to_tool_result() {
        let hook = parse(
            r#"{"hook_event_name":"PostToolUse","session_id":"s1","cwd":"/p",
                "tool_name":"Bash","tool_use_id":"t1","duration_ms":12,
                "tool_response":{"stdout":"ok"}}"#,
        );
        let ev = normalize(&hook, TS, "raw-1").unwrap();
        assert_eq!(ev.kind, EventKind::ToolResult);
        assert_eq!(ev.payload["duration_ms"], json!(12));
        assert_eq!(ev.payload["tool_response"]["stdout"], json!("ok"));
    }

    #[test]
    fn known_hook_events_map_to_expected_kinds() {
        let cases = [
            (
                r#"{"hook_event_name":"SessionStart","session_id":"s","source":"startup"}"#,
                EventKind::SessionStart,
            ),
            (
                r#"{"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"hi"}"#,
                EventKind::Prompt,
            ),
            (
                r#"{"hook_event_name":"Stop","session_id":"s","stop_hook_active":false}"#,
                EventKind::Stop,
            ),
            (
                r#"{"hook_event_name":"PostToolUseFailure","session_id":"s","tool_use_id":"t"}"#,
                EventKind::ToolFailure,
            ),
        ];
        for (line, expected) in cases {
            let ev = normalize(&parse(line), TS, "r").unwrap();
            assert_eq!(ev.kind, expected, "for {line}");
        }
    }

    #[test]
    fn missing_session_id_is_missing_field_error() {
        let hook = parse(r#"{"hook_event_name":"Stop"}"#);
        assert_eq!(
            normalize(&hook, TS, "r"),
            Err(NormalizeError::MissingField(FIELD_SESSION_ID))
        );
    }

    #[test]
    fn missing_hook_event_name_is_missing_field_error() {
        let hook = parse(r#"{"session_id":"s"}"#);
        assert_eq!(
            normalize(&hook, TS, "r"),
            Err(NormalizeError::MissingField(FIELD_HOOK_EVENT_NAME))
        );
    }

    #[test]
    fn unmapped_hook_event_is_reported() {
        let hook = parse(r#"{"hook_event_name":"Notification","session_id":"s"}"#);
        assert_eq!(
            normalize(&hook, TS, "r"),
            Err(NormalizeError::UnmappedHookEvent(
                "Notification".to_string()
            ))
        );
    }

    #[test]
    fn non_object_payload_is_rejected() {
        assert_eq!(
            normalize(&json!([1, 2]), TS, "r"),
            Err(NormalizeError::NotAnObject)
        );
    }

    #[test]
    fn error_event_is_direct_and_carries_raw_ref() {
        let ev = error_event(UNKNOWN_SESSION, TS, "raw-9", &NormalizeError::NotAnObject);
        assert_eq!(ev.kind, EventKind::Error);
        assert_eq!(ev.attribution, Attribution::Direct);
        assert_eq!(ev.session, UNKNOWN_SESSION);
        assert_eq!(ev.raw_event_ref.as_deref(), Some("raw-9"));
        assert!(ev.payload["error"].is_string());
    }
}
