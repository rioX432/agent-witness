//! Receiver: turn one raw hook payload into persisted records.
//!
//! Ties the [`crate::hooks`] normalizer to the [`crate::store`]: for each raw
//! payload it writes the verbatim raw record **and** the normalized event,
//! linked by `raw_event_ref` (canonical-source preservation, ADR-0001).
//!
//! Malformed or unmappable input never propagates as an error — it is recorded
//! as an [`crate::EventKind::Error`] event so nothing is silently dropped. The
//! receiver returns `Err` only for genuine store I/O failures.
//!
//! It is an adapter (edge), so it reads a caller-injected [`Clock`]; the pure
//! normalization it delegates to stays clock-free.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::clock::Clock;
use crate::event::{EventKind, Source};
use crate::hooks::{self, NormalizeError, UNKNOWN_SESSION};
use crate::store::{self, RawRecord, SessionStore, StoreError};
use crate::transcript::{self, TranscriptStats};

/// Prefix for generated `raw_ref` values (`raw-0`, `raw-1`, …).
const RAW_REF_PREFIX: &str = "raw-";

/// Errors from ingesting a payload. Only store I/O surfaces here; payload
/// problems are recorded as error events instead.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// The underlying session store failed (I/O, serialization).
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Outcome of a successful ingest (both records were persisted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingested {
    /// Session the records were written under.
    pub session: String,
    /// The `raw_ref` linking the raw record and its normalized event.
    pub raw_ref: String,
    /// Kind of the normalized event that was written.
    pub kind: EventKind,
}

/// Outcome of a transcript ingest. `stats` reports what the transcript parser
/// saw (honest skip accounting); `appended` / `skipped_duplicates` report what
/// this call actually wrote after idempotent de-duplication against the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptIngested {
    /// Session the supplementary events were written under.
    pub session: String,
    /// How the transcript parser classified every line.
    pub stats: TranscriptStats,
    /// Supplementary events newly appended by this call.
    pub appended: usize,
    /// Parsed events already present in the store (skipped to stay idempotent).
    pub skipped_duplicates: usize,
}

/// Ingests hook payloads into a [`SessionStore`], assigning monotonic
/// `raw_ref`s per session. Cheap to construct; keeps an in-memory per-session
/// sequence seeded lazily from what is already on disk.
#[derive(Debug)]
pub struct Receiver {
    store: SessionStore,
    next_seq: HashMap<String, u64>,
}

/// What to do with a payload once its session and validity are decided.
enum Prepared {
    /// A well-formed object with a safe session id: normalize it.
    Normalize { session: String, value: Value },
    /// Could not be normalized: record an error event under `session`.
    Error {
        session: String,
        reason: NormalizeError,
    },
}

impl Receiver {
    /// Create a receiver over `store`.
    pub fn new(store: SessionStore) -> Self {
        Self {
            store,
            next_seq: HashMap::new(),
        }
    }

    /// Borrow the underlying store (e.g. for reading back what was written).
    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// Ingest one raw payload (verbatim bytes as received on stdin / the socket).
    ///
    /// Writes the raw record and the normalized (or error) event, then returns
    /// what was persisted. `Err` only on store I/O failure.
    pub fn ingest(
        &mut self,
        raw_payload: &str,
        clock: &dyn Clock,
    ) -> Result<Ingested, IngestError> {
        let ts = clock.now_ms();
        let prepared = prepare(raw_payload);

        let session = match &prepared {
            Prepared::Normalize { session, .. } | Prepared::Error { session, .. } => {
                session.clone()
            }
        };

        let seq = self.next_seq(&session)?;
        let raw_ref = format!("{RAW_REF_PREFIX}{seq}");

        let event = match prepared {
            Prepared::Normalize { value, .. } => {
                match hooks::normalize(&value, ts, &raw_ref) {
                    Ok(ev) => ev,
                    // A well-formed object can still be unmappable (unknown hook
                    // event, missing hook_event_name): record it, don't drop it.
                    Err(reason) => hooks::error_event(&session, ts, &raw_ref, &reason),
                }
            }
            Prepared::Error { reason, .. } => hooks::error_event(&session, ts, &raw_ref, &reason),
        };

        let raw_record = RawRecord {
            v: crate::event::SCHEMA_VERSION,
            ts,
            session: session.clone(),
            raw_ref: raw_ref.clone(),
            raw: raw_payload.to_string(),
        };

        let mut writer = self.store.open(&session, ts)?;
        writer.append_raw(&raw_record)?;
        writer.append(&event)?;

        Ok(Ingested {
            session,
            raw_ref,
            kind: event.kind,
        })
    }

    /// Ingest supplementary events from a session's transcript contents.
    ///
    /// Best-effort and non-canonical: transcript events are
    /// [`crate::Attribution::Observed`], appended alongside the canonical hook
    /// events. Idempotent — events already present (matched by
    /// `raw_event_ref`) are skipped, so re-reading the same transcript (e.g. a
    /// duplicated `Stop`, or the `emit` fallback re-running after a lost ack)
    /// never double-writes. No raw record is written: the transcript file itself
    /// is the raw evidence, referenced by `transcript:<uuid>`.
    ///
    /// Returns `Err` only on genuine store I/O (including an invalid session id);
    /// a malformed transcript line is counted in the returned stats, not fatal.
    pub fn ingest_transcript(
        &mut self,
        session: &str,
        transcript_content: &str,
        clock: &dyn Clock,
    ) -> Result<TranscriptIngested, IngestError> {
        let ts = clock.now_ms();
        let read = transcript::parse_transcript(transcript_content, session, ts);

        // Seed the de-dup set from transcript events already on disk. `read`
        // validates the session id, so an unsafe id fails here (not mid-write).
        let existing = self.store.read(session)?;
        let mut seen: HashSet<String> = existing
            .events
            .iter()
            .filter(|e| e.source == Source::Transcript)
            .filter_map(|e| e.raw_event_ref.clone())
            .collect();

        let mut appended = 0;
        let mut skipped_duplicates = 0;
        if !read.events.is_empty() {
            let mut writer = self.store.open(session, ts)?;
            for event in &read.events {
                // A ref-less event cannot be de-duplicated; append it (rare).
                if let Some(reference) = &event.raw_event_ref {
                    if !seen.insert(reference.clone()) {
                        skipped_duplicates += 1;
                        continue;
                    }
                }
                writer.append(event)?;
                appended += 1;
            }
        }

        // Recompute and overwrite the per-session usage sidecar. Independent of
        // the prose parse above: a tool-only message carries no text event yet
        // still reports usage, so this scans ALL assistant lines. Whole-transcript
        // recompute + overwrite makes it idempotent by construction (unlike the
        // event append, no dedup against the store is needed).
        let usage = transcript::aggregate_transcript_usage(transcript_content, session);
        self.store.write_usage(&usage)?;

        Ok(TranscriptIngested {
            session: session.to_string(),
            stats: read.stats,
            appended,
            skipped_duplicates,
        })
    }

    /// Forget cached sequence state, forcing a disk re-seed on next ingest.
    ///
    /// A long-lived server calls this after an ingest failure: the failed seq
    /// left a gap, and the `emit` fallback may have written records directly in
    /// the meantime, so the in-memory counters can no longer be trusted.
    pub fn reset_seq_cache(&mut self) {
        self.next_seq.clear();
    }

    /// Next `raw_ref` sequence for a session, seeded from disk on first sight so
    /// a one-shot fallback writer and a long-lived server never collide.
    fn next_seq(&mut self, session: &str) -> Result<u64, IngestError> {
        let next = match self.next_seq.get(session) {
            Some(n) => *n,
            None => self.seed_from_disk(session)?,
        };
        self.next_seq
            .insert(session.to_string(), next.saturating_add(1));
        Ok(next)
    }

    /// First unused sequence: max(`raw-N` on disk) + 1 — not the line count.
    /// A gap (a seq consumed but never persisted, e.g. a transient write
    /// failure) must not make a later writer reissue an existing `raw_ref`,
    /// or the raw <-> event linkage (ADR-0001) becomes ambiguous.
    fn seed_from_disk(&self, session: &str) -> Result<u64, IngestError> {
        let read = self.store.read_raw(session)?;
        let max_seq = read
            .records
            .iter()
            .filter_map(|r| r.raw_ref.strip_prefix(RAW_REF_PREFIX)?.parse::<u64>().ok())
            .max();
        Ok(max_seq.map_or(0, |m| m.saturating_add(1)))
    }
}

/// Classify a raw payload into a normalization target or an error, choosing the
/// session it should be stored under. Untrusted `session_id`s are validated so a
/// path-traversal attempt lands in the [`UNKNOWN_SESSION`] bucket, not the store
/// error path.
fn prepare(raw_payload: &str) -> Prepared {
    let trimmed = raw_payload.trim();
    let value: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => {
            return Prepared::Error {
                session: UNKNOWN_SESSION.to_string(),
                reason: NormalizeError::MalformedJson(e.to_string()),
            }
        }
    };
    if !value.is_object() {
        return Prepared::Error {
            session: UNKNOWN_SESSION.to_string(),
            reason: NormalizeError::NotAnObject,
        };
    }
    match hooks::session_id_of(&value) {
        None => Prepared::Error {
            session: UNKNOWN_SESSION.to_string(),
            reason: NormalizeError::MissingField(hooks::FIELD_SESSION_ID),
        },
        Some(sid) if !store::is_valid_session_id(sid) => Prepared::Error {
            session: UNKNOWN_SESSION.to_string(),
            reason: NormalizeError::InvalidSessionId(sid.to_string()),
        },
        Some(sid) => Prepared::Normalize {
            session: sid.to_string(),
            value,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::event::Attribution;

    const TS: i64 = 1_700_000_000_000;

    fn receiver() -> (tempfile::TempDir, Receiver) {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        (tmp, Receiver::new(store))
    }

    #[test]
    fn ingest_pre_tool_use_writes_paired_raw_and_event() {
        let (_tmp, mut rx) = receiver();
        let clock = FixedClock(TS);
        let line = r#"{"hook_event_name":"PreToolUse","session_id":"s1","cwd":"/p","tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"ls"}}"#;

        let out = rx.ingest(line, &clock).unwrap();
        assert_eq!(out.session, "s1");
        assert_eq!(out.raw_ref, "raw-0");
        assert_eq!(out.kind, EventKind::ToolCall);

        let events = rx.store().read("s1").unwrap();
        assert_eq!(events.skipped_lines, 0);
        assert_eq!(events.events.len(), 1);
        let ev = &events.events[0];
        assert_eq!(ev.raw_event_ref.as_deref(), Some("raw-0"));
        assert_eq!(ev.attribution, Attribution::Direct);

        let raw = rx.store().read_raw("s1").unwrap();
        assert_eq!(raw.records.len(), 1);
        assert_eq!(raw.records[0].raw, line); // verbatim
        assert_eq!(raw.records[0].raw_ref, "raw-0");
    }

    #[test]
    fn raw_refs_are_monotonic_per_session() {
        let (_tmp, mut rx) = receiver();
        let clock = FixedClock(TS);
        for _ in 0..3 {
            rx.ingest(
                r#"{"hook_event_name":"Stop","session_id":"s1","stop_hook_active":false}"#,
                &clock,
            )
            .unwrap();
        }
        let refs: Vec<_> = rx
            .store()
            .read_raw("s1")
            .unwrap()
            .records
            .into_iter()
            .map(|r| r.raw_ref)
            .collect();
        assert_eq!(refs, vec!["raw-0", "raw-1", "raw-2"]);
    }

    #[test]
    fn seq_seeds_from_disk_across_receiver_instances() {
        let tmp = tempfile::TempDir::new().unwrap();
        let stop = r#"{"hook_event_name":"Stop","session_id":"s1","stop_hook_active":false}"#;
        {
            let mut rx = Receiver::new(SessionStore::new(tmp.path()));
            rx.ingest(stop, &FixedClock(TS)).unwrap();
        }
        // Fresh receiver (simulates a second one-shot `emit` fallback process).
        let mut rx2 = Receiver::new(SessionStore::new(tmp.path()));
        let out = rx2.ingest(stop, &FixedClock(TS)).unwrap();
        assert_eq!(out.raw_ref, "raw-1");
    }

    #[test]
    fn seq_seeds_from_max_not_line_count_so_gaps_never_collide() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        // Simulate a gap: raw-0 and raw-5 exist (raw-1..4 were consumed but
        // never persisted, e.g. transient write failures).
        let mut writer = store.open("s1", TS).unwrap();
        for seq in [0u64, 5] {
            writer
                .append_raw(&RawRecord {
                    v: crate::event::SCHEMA_VERSION,
                    ts: TS,
                    session: "s1".to_string(),
                    raw_ref: format!("raw-{seq}"),
                    raw: "{}".to_string(),
                })
                .unwrap();
        }
        drop(writer);

        let mut rx = Receiver::new(store);
        let out = rx
            .ingest(
                r#"{"hook_event_name":"Stop","session_id":"s1","stop_hook_active":false}"#,
                &FixedClock(TS),
            )
            .unwrap();
        // Line count is 2, but max seq is 5 — the next ref must be raw-6.
        assert_eq!(out.raw_ref, "raw-6");
    }

    #[test]
    fn reset_seq_cache_forces_disk_reseed() {
        let (_tmp, mut rx) = receiver();
        let stop = r#"{"hook_event_name":"Stop","session_id":"s1","stop_hook_active":false}"#;
        rx.ingest(stop, &FixedClock(TS)).unwrap();
        rx.reset_seq_cache();
        // Re-seeded from disk (max=0), so the next ref continues correctly.
        let out = rx.ingest(stop, &FixedClock(TS)).unwrap();
        assert_eq!(out.raw_ref, "raw-1");
    }

    #[test]
    fn malformed_json_is_recorded_as_error_event_not_a_failure() {
        let (_tmp, mut rx) = receiver();
        let out = rx.ingest("{ not json", &FixedClock(TS)).unwrap();
        assert_eq!(out.session, UNKNOWN_SESSION);
        assert_eq!(out.kind, EventKind::Error);

        let events = rx.store().read(UNKNOWN_SESSION).unwrap();
        assert_eq!(events.events.len(), 1);
        assert_eq!(events.events[0].kind, EventKind::Error);
        // The raw bytes are still preserved verbatim.
        let raw = rx.store().read_raw(UNKNOWN_SESSION).unwrap();
        assert_eq!(raw.records[0].raw, "{ not json");
    }

    #[test]
    fn missing_session_id_lands_in_unknown_bucket_as_error() {
        let (_tmp, mut rx) = receiver();
        let out = rx
            .ingest(r#"{"hook_event_name":"Stop"}"#, &FixedClock(TS))
            .unwrap();
        assert_eq!(out.session, UNKNOWN_SESSION);
        assert_eq!(out.kind, EventKind::Error);
    }

    const ASSISTANT_LINE_A: &str = r#"{"type":"assistant","uuid":"a","timestamp":"2026-07-02T02:36:29.167Z","message":{"content":[{"type":"text","text":"first"}]}}"#;
    const ASSISTANT_LINE_B: &str = r#"{"type":"assistant","uuid":"b","message":{"content":[{"type":"text","text":"second"}]}}"#;

    #[test]
    fn ingest_transcript_appends_observed_events_without_raw_records() {
        let (_tmp, mut rx) = receiver();
        let content = format!("{ASSISTANT_LINE_A}\n{ASSISTANT_LINE_B}");
        let out = rx
            .ingest_transcript("s1", &content, &FixedClock(TS))
            .unwrap();

        assert_eq!(out.appended, 2);
        assert_eq!(out.skipped_duplicates, 0);
        assert_eq!(out.stats.events_extracted, 2);

        let events = rx.store().read("s1").unwrap();
        assert_eq!(events.events.len(), 2);
        for ev in &events.events {
            assert_eq!(ev.source, crate::event::Source::Transcript);
            assert_eq!(ev.attribution, Attribution::Observed);
        }
        // The transcript file is the raw evidence; no raw records are written.
        assert!(rx.store().read_raw("s1").unwrap().records.is_empty());
    }

    #[test]
    fn ingest_transcript_is_idempotent() {
        let (_tmp, mut rx) = receiver();
        let content = format!("{ASSISTANT_LINE_A}\n{ASSISTANT_LINE_B}");
        rx.ingest_transcript("s1", &content, &FixedClock(TS))
            .unwrap();
        let second = rx
            .ingest_transcript("s1", &content, &FixedClock(TS))
            .unwrap();

        assert_eq!(second.appended, 0);
        assert_eq!(second.skipped_duplicates, 2);
        // Still only two events after re-ingesting the same transcript.
        assert_eq!(rx.store().read("s1").unwrap().events.len(), 2);
    }

    #[test]
    fn ingest_transcript_coexists_with_hook_events() {
        let (_tmp, mut rx) = receiver();
        rx.ingest(
            r#"{"hook_event_name":"Stop","session_id":"s1","stop_hook_active":false}"#,
            &FixedClock(TS),
        )
        .unwrap();
        rx.ingest_transcript("s1", ASSISTANT_LINE_A, &FixedClock(TS))
            .unwrap();

        let events = rx.store().read("s1").unwrap();
        assert_eq!(events.events.len(), 2);
        let sources: Vec<_> = events.events.iter().map(|e| e.source).collect();
        assert!(sources.contains(&crate::event::Source::Hooks));
        assert!(sources.contains(&crate::event::Source::Transcript));
    }

    #[test]
    fn ingest_transcript_rejects_invalid_session_id() {
        let (_tmp, mut rx) = receiver();
        let err = rx
            .ingest_transcript("../escape", ASSISTANT_LINE_A, &FixedClock(TS))
            .unwrap_err();
        assert!(matches!(err, IngestError::Store(_)));
    }

    #[test]
    fn ingest_transcript_with_no_events_writes_nothing() {
        let (_tmp, mut rx) = receiver();
        let out = rx
            .ingest_transcript("s1", "{broken\n{\"type\":\"user\"}", &FixedClock(TS))
            .unwrap();
        assert_eq!(out.appended, 0);
        assert_eq!(out.stats.total_lines, 2);
        assert!(rx.store().read("s1").unwrap().events.is_empty());
    }

    /// The sanitized golden transcript: msg_0001 is split across 3 lines
    /// (text + 2 tool_use) all repeating one usage; msg_0002 is one line.
    const FIXTURE_TRANSCRIPT: &str =
        include_str!("../../../tests/fixtures/session-basic/transcript.jsonl");

    #[test]
    fn ingest_transcript_writes_deduped_per_model_usage_sidecar() {
        use crate::transcript::UsageSourceStatus;

        let (_tmp, mut rx) = receiver();
        rx.ingest_transcript("session-basic", FIXTURE_TRANSCRIPT, &FixedClock(TS))
            .unwrap();

        let usage = rx
            .store()
            .read_usage("session-basic")
            .unwrap()
            .expect("usage.json written after a successful transcript read");

        assert_eq!(usage.source_status, UsageSourceStatus::Ok);
        assert_eq!(usage.attribution, Attribution::Observed);
        // msg_0001 (3 lines) + msg_0002 (1 line) => 2 unique, 2 duplicate lines.
        assert_eq!(usage.messages_counted, 2);
        assert_eq!(usage.duplicate_usage_lines_deduped, 2);
        assert_eq!(usage.messages_missing_usage, 0);
        assert_eq!(usage.assistant_lines_missing_message_id, 0);

        assert_eq!(usage.per_model.len(), 1);
        let m = &usage.per_model[0];
        assert_eq!(m.model, "claude-fable-5");
        assert_eq!(m.messages, 2);
        // Deduped sums, verified against the fixture (4116+2, 263+68, 4622+4571,
        // 15001+19623) — NOT tripled by msg_0001's repeated lines.
        assert_eq!(m.input_tokens, 4118);
        assert_eq!(m.output_tokens, 331);
        assert_eq!(m.cache_creation_input_tokens, 9193);
        assert_eq!(m.cache_read_input_tokens, 34624);
    }

    #[test]
    fn ingest_transcript_usage_sidecar_is_idempotent() {
        let (_tmp, mut rx) = receiver();
        rx.ingest_transcript("session-basic", FIXTURE_TRANSCRIPT, &FixedClock(TS))
            .unwrap();
        let first = rx.store().read_usage("session-basic").unwrap();
        // Re-ingesting the same transcript overwrites, never doubles.
        rx.ingest_transcript("session-basic", FIXTURE_TRANSCRIPT, &FixedClock(TS))
            .unwrap();
        let second = rx.store().read_usage("session-basic").unwrap();

        assert_eq!(first, second);
        let m = &second.unwrap().per_model[0];
        assert_eq!(m.input_tokens, 4118);
        assert_eq!(m.messages, 2);
    }

    #[test]
    fn ingest_transcript_without_usage_writes_no_usage_parsed_sidecar() {
        use crate::transcript::UsageSourceStatus;

        let (_tmp, mut rx) = receiver();
        // A transcript that is read but carries no usable usage still records an
        // honest negative — distinct from the file being absent ("not read").
        rx.ingest_transcript("s1", "{broken\n{\"type\":\"user\"}", &FixedClock(TS))
            .unwrap();

        let usage = rx
            .store()
            .read_usage("s1")
            .unwrap()
            .expect("sidecar written");
        assert_eq!(usage.source_status, UsageSourceStatus::NoUsageParsed);
        assert_eq!(usage.messages_counted, 0);
        assert!(usage.per_model.is_empty());
    }

    #[test]
    fn path_traversal_session_id_is_quarantined_not_stored_raw() {
        let (_tmp, mut rx) = receiver();
        let out = rx
            .ingest(
                r#"{"hook_event_name":"Stop","session_id":"../escape","stop_hook_active":false}"#,
                &FixedClock(TS),
            )
            .unwrap();
        assert_eq!(out.session, UNKNOWN_SESSION);
        assert_eq!(out.kind, EventKind::Error);
    }
}
