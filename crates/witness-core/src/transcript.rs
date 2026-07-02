//! Transcript adapter — a best-effort, versioned, secondary event source.
//!
//! Claude Code hooks are the canonical source (ADR-0001); the session transcript
//! is a supplement for context hooks never surface (intermediate assistant
//! prose). It is opt-outable and its failures are isolated: a bad line is
//! counted, never fatal, and the transcript's shape is not a stable contract, so
//! parsing is delegated to a versioned module ([`transcript_v1`]).
//!
//! Honest observation (Core Value 1): [`TranscriptStats`] surfaces how many
//! lines were skipped, so the adapter never pretends full coverage. Produced
//! events are [`crate::Attribution::Observed`], never `Direct`.

mod transcript_v1;

use crate::event::AgentEvent;

/// Version of the transcript parsing contract implemented here. Bump (and add a
/// `transcript_v2` module) when the recognized line shapes change.
pub const ADAPTER_VERSION: u32 = 1;

/// Prefix for a transcript event's `raw_event_ref`, distinguishing it from the
/// hooks receiver's `raw-N` refs and linking it to the transcript line `uuid`.
pub const TRANSCRIPT_RAW_REF_PREFIX: &str = "transcript:";

/// Confidence for a transcript-derived event. Below [`crate::CONFIDENCE_CERTAIN`]
/// on purpose: we are sure the text appears in the transcript, but the transcript
/// is a best-effort secondary source whose internal schema is not a stable
/// contract, so we do not claim hook-level certainty (ADR-0002).
pub(crate) const CONFIDENCE_TRANSCRIPT: f64 = 0.9;

/// Honest per-read accounting. The invariant
/// `total_lines == events_extracted + skipped_unparseable + skipped_unrecognized`
/// holds for every read, so skipped work is always visible, never hidden.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptStats {
    /// Non-blank lines processed.
    pub total_lines: usize,
    /// Lines that produced a supplementary event.
    pub events_extracted: usize,
    /// Lines that were not valid JSON.
    pub skipped_unparseable: usize,
    /// Valid-JSON lines whose shape this version does not extract.
    pub skipped_unrecognized: usize,
}

/// Events extracted from a transcript, plus the honest skip accounting.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptRead {
    /// Supplementary events, in transcript order.
    pub events: Vec<AgentEvent>,
    /// How the lines were classified.
    pub stats: TranscriptStats,
}

/// Parse a full transcript file's contents into supplementary events.
///
/// Pure: does no I/O and reads no wall-clock. `fallback_ts` is used only for a
/// line whose own `timestamp` cannot be parsed. Blank lines are ignored;
/// unparseable or unrecognized lines are counted, never fatal.
pub fn parse_transcript(content: &str, session: &str, fallback_ts: i64) -> TranscriptRead {
    let mut events = Vec::new();
    let mut stats = TranscriptStats::default();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        stats.total_lines += 1;
        match transcript_v1::parse_line(line, session, fallback_ts) {
            transcript_v1::LineOutcome::Event(event) => {
                events.push(*event);
                stats.events_extracted += 1;
            }
            transcript_v1::LineOutcome::Unrecognized => stats.skipped_unrecognized += 1,
            transcript_v1::LineOutcome::Unparseable => stats.skipped_unparseable += 1,
        }
    }
    TranscriptRead { events, stats }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Attribution, EventKind, Source};

    const FALLBACK: i64 = 1_700_000_000_000;

    #[test]
    fn empty_input_yields_no_events_and_zero_stats() {
        let read = parse_transcript("", "s", FALLBACK);
        assert!(read.events.is_empty());
        assert_eq!(read.stats, TranscriptStats::default());
    }

    #[test]
    fn blank_lines_are_ignored_not_counted() {
        let read = parse_transcript("\n   \n\n", "s", FALLBACK);
        assert_eq!(read.stats.total_lines, 0);
    }

    #[test]
    fn mixed_known_unknown_and_broken_lines_are_all_accounted_for() {
        let content = concat!(
            r#"{"type":"assistant","uuid":"a","timestamp":"2026-07-02T02:36:29.167Z","message":{"content":[{"type":"text","text":"hi"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"prompt"}}"#, // unrecognized
            "\n",
            "{ broken json", // unparseable
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#, // unrecognized (tool-only)
            "\n",
            r#"{"type":"assistant","uuid":"b","message":{"content":[{"type":"text","text":"done"}]}}"#,
        );
        let read = parse_transcript(content, "s", FALLBACK);

        assert_eq!(read.stats.total_lines, 5);
        assert_eq!(read.stats.events_extracted, 2);
        assert_eq!(read.stats.skipped_unparseable, 1);
        assert_eq!(read.stats.skipped_unrecognized, 2);
        // Invariant: nothing is silently lost.
        assert_eq!(
            read.stats.total_lines,
            read.stats.events_extracted
                + read.stats.skipped_unparseable
                + read.stats.skipped_unrecognized
        );
        assert_eq!(read.events.len(), 2);
        for ev in &read.events {
            assert_eq!(ev.kind, EventKind::Prompt);
            assert_eq!(ev.source, Source::Transcript);
            assert_eq!(ev.attribution, Attribution::Observed);
        }
        assert_eq!(read.events[0].payload["text"], "hi");
        assert_eq!(read.events[1].payload["text"], "done");
    }

    #[test]
    fn a_single_broken_line_does_not_abort_the_read() {
        let read = parse_transcript("{bad\n{also bad", "s", FALLBACK);
        assert_eq!(read.stats.total_lines, 2);
        assert_eq!(read.stats.skipped_unparseable, 2);
        assert!(read.events.is_empty());
    }
}
