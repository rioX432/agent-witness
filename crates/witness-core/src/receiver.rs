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

use std::collections::HashMap;

use serde_json::Value;

use crate::clock::Clock;
use crate::event::EventKind;
use crate::hooks::{self, NormalizeError, UNKNOWN_SESSION};
use crate::store::{self, RawRecord, SessionStore, StoreError};

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
