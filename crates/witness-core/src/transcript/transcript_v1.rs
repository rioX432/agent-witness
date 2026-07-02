//! Version 1 transcript line parser.
//!
//! The Claude Code transcript JSONL internal schema is **not** a stable contract
//! (CLAUDE.md, ADR-0001): this parser is best-effort and versioned so a future
//! shape change lands in a `transcript_v2` module instead of silently corrupting
//! output. It recognizes exactly one line shape today — an `assistant` message
//! carrying text content — because that is the context hooks do **not** surface
//! (hooks give only the *final* assistant message, via `Stop`). Tool calls and
//! tool results already arrive as canonical `Direct` hook events, so transcript
//! lines carrying those are deliberately skipped, not duplicated.
//!
//! Every produced event is [`Attribution::Observed`] (never `Direct`, ADR-0002):
//! the transcript is a secondary source, so we can confirm the text was written
//! but not claim the same certainty as a hook the agent reported itself.

use serde_json::{json, Map, Value};

use crate::event::{AgentEvent, Attribution, EventKind, Source};

use super::{ADAPTER_VERSION, CONFIDENCE_TRANSCRIPT, TRANSCRIPT_RAW_REF_PREFIX};

/// Transcript line `type` we extract events from.
const TYPE_ASSISTANT: &str = "assistant";
/// Content block `type` carrying assistant prose.
const BLOCK_TEXT: &str = "text";
/// Role recorded in the produced event payload.
const ROLE_ASSISTANT: &str = "assistant";
/// Separator used when an assistant turn has several text blocks.
const TEXT_JOIN: &str = "\n";

/// Result of classifying a single transcript line.
pub(crate) enum LineOutcome {
    /// A recognized line that produced one supplementary event.
    Event(Box<AgentEvent>),
    /// Valid JSON, but not a shape this version extracts (unknown `type`, a
    /// tool-only assistant turn, a user/system line, …). Counted, never claimed.
    Unrecognized,
    /// The line was not valid JSON. Counted, never claimed.
    Unparseable,
}

/// Classify and (if recognized) convert one transcript line.
///
/// `fallback_ts` is used only when the line carries no parseable `timestamp`;
/// the transcript's own timestamp is preferred so events keep their real time.
pub(crate) fn parse_line(line: &str, session: &str, fallback_ts: i64) -> LineOutcome {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return LineOutcome::Unparseable,
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => return LineOutcome::Unrecognized,
    };
    if obj.get("type").and_then(Value::as_str) != Some(TYPE_ASSISTANT) {
        return LineOutcome::Unrecognized;
    }
    let text = match assistant_text(obj) {
        Some(t) => t,
        None => return LineOutcome::Unrecognized,
    };

    let ts = obj
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(epoch_ms_from_rfc3339)
        .unwrap_or(fallback_ts);

    let payload = json!({
        "role": ROLE_ASSISTANT,
        "text": text,
        "transcript_type": TYPE_ASSISTANT,
        "adapter_version": ADAPTER_VERSION,
    });

    let mut event = AgentEvent::new(
        ts,
        session,
        Source::Transcript,
        EventKind::Prompt,
        Attribution::Observed,
        CONFIDENCE_TRANSCRIPT,
        payload,
    );
    // Link back to the transcript line so the event is traceable to its evidence
    // and deduplicable across re-reads (idempotent ingest).
    if let Some(uuid) = obj.get("uuid").and_then(Value::as_str) {
        event.raw_event_ref = Some(format!("{TRANSCRIPT_RAW_REF_PREFIX}{uuid}"));
    }
    LineOutcome::Event(Box::new(event))
}

/// Concatenate the text blocks of an assistant message, if any. Returns `None`
/// for a tool-only turn (no text) so the caller counts it as unrecognized.
fn assistant_text(obj: &Map<String, Value>) -> Option<String> {
    let content = obj.get("message")?.get("content")?.as_array()?;
    let mut parts = Vec::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some(BLOCK_TEXT) {
            if let Some(text) = block.get(BLOCK_TEXT).and_then(Value::as_str) {
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(TEXT_JOIN))
    }
}

// --- RFC 3339 timestamp parsing (deterministic, no wall-clock, no deps) -------

/// Milliseconds in a second / seconds in a minute / minute in seconds, etc.,
/// named to avoid magic numbers in the arithmetic below.
const MS_PER_SEC: i64 = 1000;
const SECS_PER_MIN: i64 = 60;
const SECS_PER_HOUR: i64 = 3600;
const SECS_PER_DAY: i64 = 86_400;
const MILLIS_FRACTION_DIGITS: usize = 3;
/// Days from 0000-03-01 to 1970-01-01 in the days-from-civil algorithm.
const CIVIL_EPOCH_SHIFT: i64 = 719_468;
const DAYS_PER_ERA: i64 = 146_097;
const YEARS_PER_ERA: i64 = 400;
/// Minimum length of a bare `YYYY-MM-DDटHH:MM:SS` timestamp.
const MIN_LEN: usize = 19;

fn ascii_digit(b: u8) -> Option<i64> {
    if b.is_ascii_digit() {
        Some((b - b'0') as i64)
    } else {
        None
    }
}

/// Two ASCII digits at `i`, `i+1` as a number.
fn two(bytes: &[u8], i: usize) -> Option<i64> {
    Some(ascii_digit(*bytes.get(i)?)? * 10 + ascii_digit(*bytes.get(i + 1)?)?)
}

/// Parse an RFC 3339 / ISO 8601 UTC-or-offset timestamp to Unix epoch
/// milliseconds. Returns `None` for anything it cannot parse (the caller then
/// falls back), so a malformed timestamp never panics or aborts a read.
fn epoch_ms_from_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < MIN_LEN {
        return None;
    }
    let year = ascii_digit(b[0])? * 1000
        + ascii_digit(b[1])? * 100
        + ascii_digit(b[2])? * 10
        + ascii_digit(b[3])?;
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let month = two(b, 5)?;
    let day = two(b, 8)?;
    if !matches!(b[10], b'T' | b't' | b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let hour = two(b, 11)?;
    let minute = two(b, 14)?;
    let second = two(b, 17)?;
    // Reject impossible field values (leap second 60 is tolerated).
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    let mut i = MIN_LEN;
    let mut millis = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let mut digits = 0usize;
        while let Some(&c) = b.get(i) {
            if !c.is_ascii_digit() {
                break;
            }
            if digits < MILLIS_FRACTION_DIGITS {
                millis = millis * 10 + (c - b'0') as i64;
                digits += 1;
            }
            i += 1;
        }
        // Left-pad a short fraction (".1" == 100ms).
        while digits < MILLIS_FRACTION_DIGITS {
            millis *= 10;
            digits += 1;
        }
    }

    let offset_secs = parse_offset(b, i)?;
    let days = days_from_civil(year, month, day);
    let secs =
        days * SECS_PER_DAY + hour * SECS_PER_HOUR + minute * SECS_PER_MIN + second - offset_secs;
    Some(secs * MS_PER_SEC + millis)
}

/// Parse the timezone designator starting at `i`: end-of-string or `Z`/`z` mean
/// UTC; `±HH:MM` / `±HHMM` is an explicit offset (returned in seconds).
fn parse_offset(b: &[u8], i: usize) -> Option<i64> {
    match b.get(i) {
        None => Some(0),
        Some(b'Z') | Some(b'z') => Some(0),
        Some(&sign @ b'+') | Some(&sign @ b'-') => {
            let oh = two(b, i + 1)?;
            // Optional ':' between hours and minutes.
            let m_start = if b.get(i + 3) == Some(&b':') {
                i + 4
            } else {
                i + 3
            };
            let om = two(b, m_start)?;
            if oh > 23 || om > 59 {
                return None;
            }
            let magnitude = oh * SECS_PER_HOUR + om * SECS_PER_MIN;
            Some(if sign == b'-' { -magnitude } else { magnitude })
        }
        _ => None,
    }
}

/// Days since 1970-01-01 for a proleptic-Gregorian date (Howard Hinnant's
/// `days_from_civil`). Unix time ignores leap seconds, so this is exact.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - (YEARS_PER_ERA - 1) }) / YEARS_PER_ERA;
    let yoe = y - era * YEARS_PER_ERA; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 }; // Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * DAYS_PER_ERA + doe - CIVIL_EPOCH_SHIFT
}

#[cfg(test)]
mod tests {
    use super::*;

    const FALLBACK: i64 = 1_700_000_000_000;

    fn as_event(line: &str) -> AgentEvent {
        match parse_line(line, "sess-1", FALLBACK) {
            LineOutcome::Event(e) => *e,
            LineOutcome::Unrecognized => panic!("expected event, got unrecognized"),
            LineOutcome::Unparseable => panic!("expected event, got unparseable"),
        }
    }

    #[test]
    fn extracts_assistant_text_as_observed_transcript_event() {
        let line = r#"{"type":"assistant","uuid":"u-9","timestamp":"2026-07-02T02:36:29.167Z",
            "message":{"role":"assistant","content":[{"type":"text","text":"planning next"}]}}"#;
        let ev = as_event(line);
        assert_eq!(ev.kind, EventKind::Prompt);
        assert_eq!(ev.source, Source::Transcript);
        assert_eq!(ev.attribution, Attribution::Observed);
        assert_eq!(ev.ts, 1_782_959_789_167); // from the transcript timestamp
        assert_eq!(ev.payload["role"], "assistant");
        assert_eq!(ev.payload["text"], "planning next");
        assert_eq!(ev.payload["adapter_version"], ADAPTER_VERSION);
        assert_eq!(ev.raw_event_ref.as_deref(), Some("transcript:u-9"));
    }

    #[test]
    fn joins_multiple_text_blocks() {
        let line = r#"{"type":"assistant","uuid":"u","timestamp":"bad",
            "message":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}}"#;
        let ev = as_event(line);
        assert_eq!(ev.payload["text"], "a\nb");
        // Unparseable timestamp -> fallback.
        assert_eq!(ev.ts, FALLBACK);
    }

    #[test]
    fn tool_only_assistant_turn_is_unrecognized() {
        // Tool calls arrive as canonical Direct hook events; do not duplicate.
        let line = r#"{"type":"assistant","uuid":"u",
            "message":{"content":[{"type":"tool_use","name":"Bash","input":{}}]}}"#;
        assert!(matches!(
            parse_line(line, "s", FALLBACK),
            LineOutcome::Unrecognized
        ));
    }

    #[test]
    fn non_assistant_types_are_unrecognized() {
        for line in [
            r#"{"type":"user","message":{"content":"hi"}}"#,
            r#"{"type":"queue-operation","operation":"enqueue"}"#,
            r#"{"type":"last-prompt","lastPrompt":"x"}"#,
            r#"{"no_type":true}"#,
            r#"[1,2,3]"#,
        ] {
            assert!(
                matches!(parse_line(line, "s", FALLBACK), LineOutcome::Unrecognized),
                "expected unrecognized for {line}"
            );
        }
    }

    #[test]
    fn invalid_json_is_unparseable() {
        assert!(matches!(
            parse_line("{ not json", "s", FALLBACK),
            LineOutcome::Unparseable
        ));
    }

    #[test]
    fn rfc3339_parses_known_instants() {
        assert_eq!(epoch_ms_from_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            epoch_ms_from_rfc3339("2000-01-01T00:00:00.000Z"),
            Some(946_684_800_000)
        );
        assert_eq!(
            epoch_ms_from_rfc3339("2026-07-02T02:36:29.167Z"),
            Some(1_782_959_789_167)
        );
        // Leap day, single fractional digit (.5 == 500ms).
        assert_eq!(
            epoch_ms_from_rfc3339("2024-02-29T12:00:00.5Z"),
            Some(1_709_208_000_500)
        );
    }

    #[test]
    fn rfc3339_applies_explicit_offset() {
        // +02:00 is two hours ahead of UTC, so the epoch is two hours earlier.
        assert_eq!(
            epoch_ms_from_rfc3339("2026-07-02T00:00:00+02:00"),
            Some(1_782_943_200_000)
        );
        assert_eq!(
            epoch_ms_from_rfc3339("2026-07-02T00:00:00+0200"),
            Some(1_782_943_200_000)
        );
    }

    #[test]
    fn rfc3339_rejects_garbage() {
        for bad in ["", "not-a-date", "2026/07/02 x", "2026-13-01T00:00:00Z"] {
            assert_eq!(epoch_ms_from_rfc3339(bad), None, "should reject {bad}");
        }
    }
}
