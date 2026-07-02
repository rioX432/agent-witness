//! Fixture-driven test for the transcript adapter (issue #4).
//!
//! Runs the real, sanitized `session-basic` transcript through the versioned
//! parser and asserts the supplementary events it extracts (assistant prose that
//! hooks never surface), the honest skip accounting, and — as a sanitization
//! gate — that no real path or id leaked into the committed fixture.

use std::path::{Path, PathBuf};

use agent_witness_core::{parse_transcript, Attribution, EventKind, Source};

const FALLBACK_TS: i64 = 1_700_000_000_000;

fn transcript_path(scenario: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join(scenario)
        .join("transcript.jsonl")
}

fn read_fixture(scenario: &str) -> String {
    let path = transcript_path(scenario);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn extracts_assistant_prose_from_real_transcript() {
    let content = read_fixture("session-basic");
    let read = parse_transcript(&content, "session-basic", FALLBACK_TS);

    // Two assistant text turns; the rest (tool_use turns, tool_result users, the
    // user prompt, queue-operation, last-prompt) are recognized-but-not-emitted.
    assert_eq!(read.events.len(), 2, "expected two assistant text events");
    assert_eq!(read.stats.events_extracted, 2);
    assert_eq!(
        read.stats.skipped_unparseable, 0,
        "fixture must be valid JSON"
    );
    assert_eq!(read.stats.total_lines, 9);
    // Honest-accounting invariant: nothing is silently dropped.
    assert_eq!(
        read.stats.total_lines,
        read.stats.events_extracted
            + read.stats.skipped_unparseable
            + read.stats.skipped_unrecognized
    );

    for ev in &read.events {
        assert_eq!(ev.kind, EventKind::Prompt);
        assert_eq!(ev.source, Source::Transcript);
        // Never Direct: transcript is a secondary source (ADR-0002).
        assert_eq!(ev.attribution, Attribution::Observed);
        assert_eq!(ev.payload["role"], "assistant");
        assert!(ev.payload["text"].as_str().is_some_and(|t| !t.is_empty()));
        assert!(ev
            .raw_event_ref
            .as_deref()
            .is_some_and(|r| r.starts_with("transcript:")));
    }

    // The first event is intermediate prose hooks never emit (only Stop carries
    // the final assistant message), demonstrating the adapter's value.
    let first = read.events[0].payload["text"].as_str().unwrap();
    assert!(
        first.contains("hello.rs"),
        "unexpected first assistant text: {first}"
    );
    // Its timestamp came from the transcript line, not the fallback.
    assert_ne!(read.events[0].ts, FALLBACK_TS);
}

/// Sanitization gate: the committed transcript fixture must carry no real
/// machine paths or raw hex UUIDs (honest-observation Core Value).
#[test]
fn transcript_fixture_is_sanitized() {
    let content = read_fixture("session-basic");
    for forbidden in ["/Users/", "/private/tmp", "/var/folders", "sk-ant-"] {
        assert!(
            !content.contains(forbidden),
            "transcript fixture leaked {forbidden:?}"
        );
    }
    // Raw 8-4-4-4-12 hex UUIDs are replaced by non-hex placeholders (uuid-NNNN).
    for line in content.lines() {
        assert!(
            !contains_hex_uuid(line),
            "transcript fixture has an unsanitized hex UUID: {line}"
        );
    }
}

/// Detect a raw `8-4-4-4-12` hex UUID (mirrors the hooks fixture lint).
fn contains_hex_uuid(s: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    const TOTAL: usize = 36;
    let b = s.as_bytes();
    if b.len() < TOTAL {
        return false;
    }
    'start: for start in 0..=b.len() - TOTAL {
        let mut i = start;
        for (gi, &g) in GROUPS.iter().enumerate() {
            for _ in 0..g {
                if !b[i].is_ascii_hexdigit() {
                    continue 'start;
                }
                i += 1;
            }
            if gi + 1 < GROUPS.len() {
                if b[i] != b'-' {
                    continue 'start;
                }
                i += 1;
            }
        }
        return true;
    }
    false
}
