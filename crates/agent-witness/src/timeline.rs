//! Pure grouping of normalized [`AgentEvent`]s into a replayable timeline.
//!
//! The core reduction: a `PreToolUse` ([`EventKind::ToolCall`]) and its matching
//! `PostToolUse` ([`EventKind::ToolResult`] / [`EventKind::ToolFailure`], paired
//! by `tool_use_id`) collapse into a single [`TimelineEntry`]. Every other event
//! maps one-to-one. This module does no I/O and reads no clock, so the timeline
//! is a deterministic function of its input events — the property the golden
//! screen tests depend on.
//!
//! Honesty (ADR-0002): a tool call with no matching result is not silently
//! dropped or assumed successful. It surfaces as [`ToolStatus::NoResult`],
//! reflecting the empirical reality that a failed `Bash` fires no `PostToolUse`
//! hook (see the normalizer docs), so absence of a result is genuinely
//! ambiguous and is labelled as such rather than as a success or a failure.

use std::collections::HashMap;

use agent_witness_core::{AgentEvent, Attribution, EventKind, Source};
use serde_json::Value;

/// Outcome of a tool call as observed from the hook stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// Completed with a `PostToolUse` result.
    Ok,
    /// Reported an explicit `PostToolUseFailure`.
    Failed,
    /// No matching `PostToolUse` was observed. Honest ambiguity: could be a
    /// failed `Bash` (which emits no result hook), an in-progress call in a live
    /// session, or a missing hook — never assumed to be a success.
    NoResult,
}

impl ToolStatus {
    /// Short, fixed-width-friendly label for the timeline row.
    pub fn label(self) -> &'static str {
        match self {
            ToolStatus::Ok => "ok",
            ToolStatus::Failed => "failed",
            ToolStatus::NoResult => "no-result",
        }
    }
}

/// One row in the timeline.
#[derive(Debug, Clone)]
pub struct TimelineEntry {
    /// Start time of this entry, Unix epoch milliseconds (from the source event).
    pub ts: i64,
    /// Attribution of the primary event (ADR-0002).
    pub attribution: Attribution,
    /// Origin of the primary event.
    pub source: Source,
    /// `Some` for tool-call rows; `None` for session/prompt/stop/error rows.
    pub tool_status: Option<ToolStatus>,
    /// Elapsed time of the tool call, if a result reported `duration_ms`.
    pub duration_ms: Option<i64>,
    /// Short tag shown in the timeline (tool name, or `SESSION`/`PROMPT`/…).
    pub tag: String,
    /// One-line human summary (file path, command, prompt text, …).
    pub summary: String,
    /// The primary event, kept for the detail pane.
    pub call: AgentEvent,
    /// The paired result event (a tool's `PostToolUse`), if any.
    pub result: Option<AgentEvent>,
}

/// Group events (in file order) into timeline entries.
pub fn build_timeline(events: &[AgentEvent]) -> Vec<TimelineEntry> {
    // Map each result's tool_use_id to its position, so a call can find its
    // result in one pass. First result wins if ids ever repeat.
    let mut result_by_id: HashMap<&str, usize> = HashMap::new();
    for (i, ev) in events.iter().enumerate() {
        if matches!(ev.kind, EventKind::ToolResult | EventKind::ToolFailure) {
            if let Some(id) = str_field(ev, FIELD_TOOL_USE_ID) {
                result_by_id.entry(id).or_insert(i);
            }
        }
    }

    let mut consumed = vec![false; events.len()];
    let mut entries = Vec::with_capacity(events.len());

    for (i, ev) in events.iter().enumerate() {
        match ev.kind {
            EventKind::ToolCall => {
                let result_idx =
                    str_field(ev, FIELD_TOOL_USE_ID).and_then(|id| result_by_id.get(id).copied());
                let (status, duration, result) = match result_idx {
                    Some(ri) => {
                        consumed[ri] = true;
                        let r = &events[ri];
                        let status = if r.kind == EventKind::ToolFailure {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Ok
                        };
                        (status, i64_field(r, FIELD_DURATION_MS), Some(r.clone()))
                    }
                    None => (ToolStatus::NoResult, None, None),
                };
                entries.push(TimelineEntry {
                    ts: ev.ts,
                    attribution: ev.attribution,
                    source: ev.source,
                    tool_status: Some(status),
                    duration_ms: duration,
                    tag: tool_tag(ev),
                    summary: tool_summary(ev),
                    call: ev.clone(),
                    result,
                });
            }
            EventKind::ToolResult | EventKind::ToolFailure => {
                // A result already folded into its call is skipped; a result with
                // no preceding call is surfaced on its own rather than dropped.
                if consumed[i] {
                    continue;
                }
                entries.push(TimelineEntry {
                    ts: ev.ts,
                    attribution: ev.attribution,
                    source: ev.source,
                    tool_status: Some(if ev.kind == EventKind::ToolFailure {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Ok
                    }),
                    duration_ms: i64_field(ev, FIELD_DURATION_MS),
                    tag: tool_tag(ev),
                    summary: tool_summary(ev),
                    call: ev.clone(),
                    result: None,
                });
            }
            _ => entries.push(TimelineEntry {
                ts: ev.ts,
                attribution: ev.attribution,
                source: ev.source,
                tool_status: None,
                duration_ms: None,
                tag: meta_tag(ev.kind),
                summary: meta_summary(ev),
                call: ev.clone(),
                result: None,
            }),
        }
    }

    entries
}

/// Number of tool invocations (`ToolCall` events) in a slice.
pub fn tool_call_count(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|e| e.kind == EventKind::ToolCall)
        .count()
}

// Payload field names carried by normalized events.
const FIELD_TOOL_USE_ID: &str = "tool_use_id";
const FIELD_TOOL_NAME: &str = "tool_name";
const FIELD_TOOL_INPUT: &str = "tool_input";
const FIELD_DURATION_MS: &str = "duration_ms";

/// Tag for a tool row: the tool name, or a neutral fallback if absent.
fn tool_tag(ev: &AgentEvent) -> String {
    str_field(ev, FIELD_TOOL_NAME).unwrap_or("tool").to_string()
}

/// Tag for a non-tool row.
fn meta_tag(kind: EventKind) -> String {
    match kind {
        EventKind::SessionStart => "SESSION",
        EventKind::Prompt => "PROMPT",
        EventKind::Stop => "STOP",
        EventKind::Error => "ERROR",
        // Tool kinds are handled before this is reached.
        EventKind::ToolCall | EventKind::ToolResult | EventKind::ToolFailure => "TOOL",
    }
    .to_string()
}

/// One-line summary for a tool row: the most informative `tool_input` field.
fn tool_summary(ev: &AgentEvent) -> String {
    let input = ev.payload.get(FIELD_TOOL_INPUT);
    // Ordered by how directly each names the tool's target.
    const KEYS: &[&str] = &[
        "file_path",
        "command",
        "path",
        "pattern",
        "url",
        "description",
    ];
    for key in KEYS {
        if let Some(value) = input.and_then(|v| v.get(*key)).and_then(Value::as_str) {
            return one_line(value);
        }
    }
    String::new()
}

/// One-line summary for a non-tool row.
fn meta_summary(ev: &AgentEvent) -> String {
    let key = match ev.kind {
        EventKind::SessionStart => "source",
        EventKind::Prompt => "prompt",
        EventKind::Stop => "last_assistant_message",
        EventKind::Error => "error",
        _ => return String::new(),
    };
    ev.payload
        .get(key)
        .and_then(Value::as_str)
        .map(one_line)
        .unwrap_or_default()
}

/// Read a string payload field.
fn str_field<'a>(ev: &'a AgentEvent, key: &str) -> Option<&'a str> {
    ev.payload.get(key).and_then(Value::as_str)
}

/// Read an integer payload field.
fn i64_field(ev: &AgentEvent, key: &str) -> Option<i64> {
    ev.payload.get(key).and_then(Value::as_i64)
}

/// Flatten a value to a single physical line: newlines, carriage returns, and
/// tabs become spaces so it can never break a one-line timeline row. Rendering
/// width clipping is left to ratatui.
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_witness_core::{Source, CONFIDENCE_CERTAIN};
    use serde_json::json;

    fn ev(ts: i64, kind: EventKind, payload: Value) -> AgentEvent {
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

    #[test]
    fn pairs_call_with_matching_result_into_one_entry() {
        let events = vec![
            ev(
                1,
                EventKind::ToolCall,
                json!({"tool_name": "Bash", "tool_use_id": "t1", "tool_input": {"command": "ls"}}),
            ),
            ev(
                2,
                EventKind::ToolResult,
                json!({"tool_name": "Bash", "tool_use_id": "t1", "duration_ms": 42}),
            ),
        ];
        let timeline = build_timeline(&events);
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].tool_status, Some(ToolStatus::Ok));
        assert_eq!(timeline[0].duration_ms, Some(42));
        assert_eq!(timeline[0].tag, "Bash");
        assert_eq!(timeline[0].summary, "ls");
        assert!(timeline[0].result.is_some());
    }

    #[test]
    fn unpaired_call_is_marked_no_result_not_success() {
        let events = vec![ev(
            1,
            EventKind::ToolCall,
            json!({"tool_name": "Bash", "tool_use_id": "t1", "tool_input": {"command": "cat x"}}),
        )];
        let timeline = build_timeline(&events);
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].tool_status, Some(ToolStatus::NoResult));
        assert_eq!(timeline[0].duration_ms, None);
        assert!(timeline[0].result.is_none());
    }

    #[test]
    fn explicit_failure_result_marks_failed() {
        let events = vec![
            ev(
                1,
                EventKind::ToolCall,
                json!({"tool_name": "Bash", "tool_use_id": "t9", "tool_input": {"command": "boom"}}),
            ),
            ev(
                2,
                EventKind::ToolFailure,
                json!({"tool_name": "Bash", "tool_use_id": "t9"}),
            ),
        ];
        let timeline = build_timeline(&events);
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].tool_status, Some(ToolStatus::Failed));
    }

    #[test]
    fn meta_events_map_one_to_one_with_summaries() {
        let events = vec![
            ev(
                1,
                EventKind::SessionStart,
                json!({"source": "startup", "cwd": "/p"}),
            ),
            ev(2, EventKind::Prompt, json!({"prompt": "do a\nthing"})),
            ev(
                3,
                EventKind::Stop,
                json!({"last_assistant_message": "done"}),
            ),
        ];
        let timeline = build_timeline(&events);
        assert_eq!(timeline.len(), 3);
        assert_eq!(timeline[0].tag, "SESSION");
        assert_eq!(timeline[0].summary, "startup");
        assert_eq!(timeline[1].tag, "PROMPT");
        // Newline flattened to a space so it can't break a row.
        assert_eq!(timeline[1].summary, "do a thing");
        assert_eq!(timeline[2].tag, "STOP");
        assert_eq!(timeline[2].summary, "done");
    }

    #[test]
    fn orphan_result_is_surfaced_not_dropped() {
        let events = vec![ev(
            1,
            EventKind::ToolResult,
            json!({"tool_name": "Bash", "tool_use_id": "ghost", "duration_ms": 5}),
        )];
        let timeline = build_timeline(&events);
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].tool_status, Some(ToolStatus::Ok));
    }

    #[test]
    fn tool_call_count_counts_only_calls() {
        let events = vec![
            ev(1, EventKind::SessionStart, json!({})),
            ev(2, EventKind::ToolCall, json!({"tool_use_id": "a"})),
            ev(3, EventKind::ToolResult, json!({"tool_use_id": "a"})),
            ev(4, EventKind::ToolCall, json!({"tool_use_id": "b"})),
        ];
        assert_eq!(tool_call_count(&events), 2);
    }

    #[test]
    fn empty_input_yields_empty_timeline() {
        assert!(build_timeline(&[]).is_empty());
    }
}
