//! The `AgentEvent` model — the single normalized record for everything the
//! recorder observes. One `AgentEvent` serializes to exactly one JSONL line.
//!
//! Honesty is a Core Value (see ADR-0002): every event carries an
//! [`Attribution`] and a `confidence`, and never claims more than was observed.

use serde::{Deserialize, Serialize};

/// Current schema version. Bump on breaking changes to [`AgentEvent`] so older
/// records remain identifiable and future readers can migrate.
pub const SCHEMA_VERSION: u32 = 1;

/// Full confidence (1.0). Use for facts the source reports directly.
pub const CONFIDENCE_CERTAIN: f64 = 1.0;

/// How strongly we can claim the agent caused this event (ADR-0002).
///
/// Overclaiming is the one unforgivable bug: prefer a weaker variant when in
/// doubt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attribution {
    /// Reported by the agent itself (e.g. a Claude Code hook payload). The
    /// canonical, highest-trust source in v0.1.
    Direct,
    /// Observed out-of-band (e.g. a filesystem or process change) and confirmed
    /// to have happened, but not proven to be caused by the agent.
    Observed,
    /// Correlated within a time window; a best-effort guess, not a fact.
    Inferred,
}

/// Where a record originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Claude Code hooks over the `emit` bridge — canonical source (ADR-0001).
    Hooks,
    /// The session transcript file — best-effort, versioned secondary adapter.
    Transcript,
}

/// What kind of thing happened. Fine-grained details (e.g. which tool, which
/// file) live in [`AgentEvent::payload`]; the enum stays small and stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A session began.
    SessionStart,
    /// A user/agent prompt turn.
    Prompt,
    /// A tool invocation was requested/started (Claude Code `PreToolUse`). Tool
    /// name and arguments are in the payload; there is no per-tool enum variant
    /// on purpose.
    ToolCall,
    /// A tool invocation completed and reported a response (`PostToolUse`).
    /// Pairs with a preceding [`EventKind::ToolCall`] via `tool_use_id`.
    ToolResult,
    /// A tool invocation failed. Reserved from the start (see ADR-0001).
    ///
    /// Important empirical reality (pinned by a fixture test in issue #8): a
    /// `Bash` tool call that exits non-zero fires **no** `PostToolUse` hook, so
    /// today a failure surfaces only as a [`EventKind::ToolCall`] with no
    /// matching [`EventKind::ToolResult`]. This variant is emitted only if
    /// Claude Code ever sends an explicit `PostToolUseFailure` hook; the
    /// unpaired-`ToolCall` signal is what the pipeline relies on now.
    ToolFailure,
    /// The agent stopped / a turn ended.
    Stop,
    /// The session terminated (Claude Code `SessionEnd`). Unlike [`Stop`],
    /// which fires per turn, this fires once at real session end and carries a
    /// `reason` (`clear` / `logout` / `prompt_input_exit` / ...) in the
    /// payload. A resumed session can still append events after one.
    ///
    /// [`Stop`]: EventKind::Stop
    SessionEnd,
    /// The recorder itself could not process an input (e.g. malformed hook
    /// JSON, a non-object payload, a missing required field, or an unmapped
    /// hook event). Recorded rather than dropped so nothing is silently lost.
    Error,
}

/// One normalized record. Serializes to a single JSONL line.
///
/// The type is a plain data carrier: constructing and (de)serializing it does
/// no I/O and reads no wall-clock, so it is deterministic and cheap to test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEvent {
    /// Schema version. Written as [`SCHEMA_VERSION`] for new events.
    pub v: u32,
    /// Event time, Unix epoch milliseconds. Supplied by the caller (inject a
    /// clock — never read the wall-clock inside core logic).
    pub ts: i64,
    /// Session id this event belongs to.
    pub session: String,
    /// Origin of the record.
    pub source: Source,
    /// What happened.
    pub kind: EventKind,
    /// Attribution strength (ADR-0002).
    pub attribution: Attribution,
    /// Confidence in this record, `0.0..=1.0`.
    pub confidence: f64,
    /// For correlation-based events, the window (ms) used to associate cause and
    /// effect. `None` for directly reported events.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub correlation_window_ms: Option<u64>,
    /// Back-reference to the raw source record (e.g. a raw hook payload id), so
    /// a normalized event can always be traced to its evidence.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub raw_event_ref: Option<String>,
    /// Kind-specific details (tool name, args, prompt text, …), kept as opaque
    /// JSON so the schema can evolve without churning this struct.
    pub payload: serde_json::Value,
}

impl AgentEvent {
    /// Build an event at the current [`SCHEMA_VERSION`] with no correlation
    /// metadata. `ts` must be supplied by the caller (inject a clock).
    pub fn new(
        ts: i64,
        session: impl Into<String>,
        source: Source,
        kind: EventKind,
        attribution: Attribution,
        confidence: f64,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            v: SCHEMA_VERSION,
            ts,
            session: session.into(),
            source,
            kind,
            attribution,
            confidence,
            correlation_window_ms: None,
            raw_event_ref: None,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample(kind: EventKind, attribution: Attribution) -> AgentEvent {
        AgentEvent::new(
            1_700_000_000_000,
            "sess-1",
            Source::Hooks,
            kind,
            attribution,
            CONFIDENCE_CERTAIN,
            json!({"tool_name": "Bash", "command": "ls"}),
        )
    }

    #[test]
    fn round_trip_preserves_all_fields_for_every_kind() {
        let kinds = [
            EventKind::SessionStart,
            EventKind::Prompt,
            EventKind::ToolCall,
            EventKind::Stop,
        ];
        for kind in kinds {
            let event = sample(kind, Attribution::Direct);
            let line = serde_json::to_string(&event).expect("serialize");
            let back: AgentEvent = serde_json::from_str(&line).expect("deserialize");
            assert_eq!(event, back);
        }
    }

    #[test]
    fn round_trip_preserves_correlation_metadata() {
        let mut event = sample(EventKind::ToolCall, Attribution::Inferred);
        event.correlation_window_ms = Some(500);
        event.raw_event_ref = Some("hook-42".to_string());
        let line = serde_json::to_string(&event).expect("serialize");
        let back: AgentEvent = serde_json::from_str(&line).expect("deserialize");
        assert_eq!(event, back);
    }

    #[test]
    fn attribution_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&Attribution::Direct).unwrap(),
            "\"direct\""
        );
        assert_eq!(
            serde_json::to_string(&Attribution::Observed).unwrap(),
            "\"observed\""
        );
        assert_eq!(
            serde_json::to_string(&Attribution::Inferred).unwrap(),
            "\"inferred\""
        );
    }

    #[test]
    fn kind_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&EventKind::SessionStart).unwrap(),
            "\"session_start\""
        );
        assert_eq!(
            serde_json::to_string(&EventKind::ToolCall).unwrap(),
            "\"tool_call\""
        );
    }

    #[test]
    fn new_stamps_current_schema_version() {
        let event = sample(EventKind::Prompt, Attribution::Direct);
        assert_eq!(event.v, SCHEMA_VERSION);
    }

    #[test]
    fn optional_fields_are_omitted_when_absent() {
        let event = sample(EventKind::Stop, Attribution::Observed);
        let line = serde_json::to_string(&event).unwrap();
        assert!(!line.contains("correlation_window_ms"));
        assert!(!line.contains("raw_event_ref"));
    }
}
