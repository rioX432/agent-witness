//! Append-only JSONL session store.
//!
//! Layout (the sessions root is injected — core never hardcodes `$HOME`):
//!
//! ```text
//! <sessions_root>/<session-id>/events.jsonl   # one AgentEvent per line
//! <sessions_root>/<session-id>/meta.json      # session metadata
//! ```
//!
//! Writes go through a single [`SessionWriter`] per session that owns the file
//! handle in append mode (v0.1 is synchronous; the API is shaped so the writer
//! can move behind a task later without changing callers).

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::event::{AgentEvent, SCHEMA_VERSION};
use crate::transcript::SessionUsage;

/// File name for the per-session event log.
const EVENTS_FILE: &str = "events.jsonl";
/// File name for the per-session metadata.
const META_FILE: &str = "meta.json";
/// File name for the per-session raw source log (canonical-source preservation,
/// ADR-0001): every raw hook payload is kept verbatim alongside its normalized
/// event, linked by `raw_event_ref`.
const RAW_FILE: &str = "raw.jsonl";
/// File name for the per-session usage sidecar. Derived data: recomputed from the
/// whole transcript and overwritten on each ingest, written atomically.
const USAGE_FILE: &str = "usage.json";
/// Temporary sibling of [`USAGE_FILE`]; written then renamed so a reader never
/// observes a torn file.
const USAGE_TMP_FILE: &str = "usage.json.tmp";

/// Errors from the session store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Filesystem I/O failed.
    #[error("session store I/O error at {path}: {source}")]
    Io {
        /// Path being operated on when the error occurred.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// (De)serialization of an event or metadata failed.
    #[error("session store serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    /// The session id is not a single safe path component. Rejected to prevent
    /// path traversal, since session ids originate from external hook payloads.
    #[error("invalid session id: {0:?}")]
    InvalidSessionId(String),
}

impl StoreError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Per-session metadata persisted as `meta.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Schema version this session was created under.
    pub v: u32,
    /// Session id (matches the directory name).
    pub session_id: String,
    /// Session creation time, Unix epoch milliseconds. Supplied by the caller.
    pub created_ts: i64,
}

/// A raw source record: one hook payload preserved verbatim (ADR-0001).
///
/// The original bytes are stored as a string in [`RawRecord::raw`] rather than a
/// re-serialized `Value`, so key order and formatting are preserved exactly and
/// the canonical source is never silently rewritten. Each record is linked to
/// its normalized [`AgentEvent`] by matching `raw_ref` == `AgentEvent.raw_event_ref`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawRecord {
    /// Schema version this record was written under.
    pub v: u32,
    /// Receive time, Unix epoch milliseconds. Supplied by the caller.
    pub ts: i64,
    /// Session id this raw payload was attributed to.
    pub session: String,
    /// Stable id linking this raw record to its normalized event.
    pub raw_ref: String,
    /// The raw hook payload, verbatim as received on stdin / the socket.
    pub raw: String,
}

/// Result of reading a session's event log.
///
/// `skipped_lines` is reported rather than hidden: a corrupted line is
/// unreadable, but honesty (ADR-0002) means we surface how many we dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRead {
    /// Successfully parsed events, in file order.
    pub events: Vec<AgentEvent>,
    /// Number of non-empty lines that failed to parse and were skipped.
    pub skipped_lines: usize,
}

/// Result of reading a session's raw source log, with the same honest
/// skipped-line accounting as [`SessionRead`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRead {
    /// Successfully parsed raw records, in file order.
    pub records: Vec<RawRecord>,
    /// Number of non-empty lines that failed to parse and were skipped.
    pub skipped_lines: usize,
}

/// A store rooted at a sessions directory. Cheap to clone; holds no handles.
#[derive(Debug, Clone)]
pub struct SessionStore {
    sessions_root: PathBuf,
}

impl SessionStore {
    /// Create a store rooted at `sessions_root` (e.g.
    /// `~/.agent-witness/sessions`, resolved by the caller). The directory is
    /// created lazily on first [`SessionStore::open`].
    pub fn new(sessions_root: impl Into<PathBuf>) -> Self {
        Self {
            sessions_root: sessions_root.into(),
        }
    }

    /// Directory for a given session. `session_id` must already be validated by
    /// [`validate_session_id`].
    fn session_dir(&self, session_id: &str) -> PathBuf {
        self.sessions_root.join(session_id)
    }

    /// Open (creating if needed) a session for appending.
    ///
    /// Creates the session directory and, if absent, writes `meta.json` stamped
    /// with `created_ts`. Re-opening an existing session preserves the original
    /// metadata, so this is idempotent. Returns a single-writer handle over the
    /// append-only event log.
    pub fn open(&self, session_id: &str, created_ts: i64) -> Result<SessionWriter, StoreError> {
        validate_session_id(session_id)?;
        let dir = self.session_dir(session_id);
        fs::create_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;

        let meta_path = dir.join(META_FILE);
        if !meta_path.exists() {
            let meta = SessionMeta {
                v: SCHEMA_VERSION,
                session_id: session_id.to_string(),
                created_ts,
            };
            let json = serde_json::to_string_pretty(&meta)?;
            fs::write(&meta_path, json).map_err(|e| StoreError::io(&meta_path, e))?;
        }

        let events_path = dir.join(EVENTS_FILE);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&events_path)
            .map_err(|e| StoreError::io(&events_path, e))?;

        let raw_path = dir.join(RAW_FILE);
        let raw_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&raw_path)
            .map_err(|e| StoreError::io(&raw_path, e))?;

        Ok(SessionWriter {
            events_path,
            file,
            raw_path,
            raw_file,
        })
    }

    /// Read and parse a session's event log, skipping corrupted lines so one
    /// bad line never blocks the rest. Returns an empty read if the log does
    /// not exist yet.
    pub fn read(&self, session_id: &str) -> Result<SessionRead, StoreError> {
        validate_session_id(session_id)?;
        let events_path = self.session_dir(session_id).join(EVENTS_FILE);
        read_events_file(&events_path)
    }

    /// List the ids of all stored sessions, sorted ascending.
    ///
    /// Enumerates immediate sub-directories of the sessions root whose names are
    /// valid session ids. A missing root reads as no sessions (not an error), so
    /// this is safe to call before anything has been recorded.
    pub fn list_sessions(&self) -> Result<Vec<String>, StoreError> {
        let entries = match fs::read_dir(&self.sessions_root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(StoreError::io(&self.sessions_root, e)),
        };

        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| StoreError::io(&self.sessions_root, e))?;
            let file_type = entry
                .file_type()
                .map_err(|e| StoreError::io(entry.path(), e))?;
            if !file_type.is_dir() {
                continue;
            }
            // Only surface directories whose names are valid session ids, so a
            // stray file or unrelated directory never masquerades as a session.
            if let Some(name) = entry.file_name().to_str() {
                if is_valid_session_id(name) {
                    ids.push(name.to_string());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Load a session's metadata.
    pub fn read_meta(&self, session_id: &str) -> Result<SessionMeta, StoreError> {
        validate_session_id(session_id)?;
        let meta_path = self.session_dir(session_id).join(META_FILE);
        let bytes = fs::read(&meta_path).map_err(|e| StoreError::io(&meta_path, e))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Read and parse a session's raw source log. Corrupted lines are skipped
    /// but counted ([`RawRead::skipped_lines`]) — the canonical source must
    /// never under-report silently (ADR-0002). Returns an empty read if the
    /// log does not exist yet.
    pub fn read_raw(&self, session_id: &str) -> Result<RawRead, StoreError> {
        validate_session_id(session_id)?;
        let raw_path = self.session_dir(session_id).join(RAW_FILE);
        let (records, skipped_lines) = read_jsonl(&raw_path)?;
        Ok(RawRead {
            records,
            skipped_lines,
        })
    }

    /// Write (overwriting) a session's `usage.json` sidecar atomically.
    ///
    /// The record is derived data recomputed from the whole transcript, so a
    /// full overwrite is idempotent by construction (no cross-reingest dedup
    /// needed). Written to a `.tmp` sibling then renamed into place so a
    /// concurrent reader never sees a half-written file. The session directory
    /// is created if absent (usage can precede any hook event).
    pub fn write_usage(&self, usage: &SessionUsage) -> Result<(), StoreError> {
        validate_session_id(&usage.session)?;
        let dir = self.session_dir(&usage.session);
        fs::create_dir_all(&dir).map_err(|e| StoreError::io(&dir, e))?;

        let tmp_path = dir.join(USAGE_TMP_FILE);
        let final_path = dir.join(USAGE_FILE);
        let json = serde_json::to_string_pretty(usage)?;
        fs::write(&tmp_path, json).map_err(|e| StoreError::io(&tmp_path, e))?;
        fs::rename(&tmp_path, &final_path).map_err(|e| StoreError::io(&final_path, e))?;
        Ok(())
    }

    /// Read a session's `usage.json` sidecar.
    ///
    /// Returns `Ok(None)` when the file is absent — absence means "usage
    /// unavailable / no claim", never "0 tokens used". A malformed/unreadable
    /// `usage.json` also degrades to `Ok(None)` (skip-and-continue, ADR-0002) so
    /// a future cross-session digest never fails wholesale on one bad sidecar.
    pub fn read_usage(&self, session_id: &str) -> Result<Option<SessionUsage>, StoreError> {
        validate_session_id(session_id)?;
        let usage_path = self.session_dir(session_id).join(USAGE_FILE);
        let bytes = match fs::read(&usage_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::io(&usage_path, e)),
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }
}

/// Whether `session_id` is a safe single path component (public form of the
/// internal guard). Lets callers pre-validate untrusted ids from hook payloads
/// and fall back gracefully instead of hitting a store error.
pub fn is_valid_session_id(session_id: &str) -> bool {
    validate_session_id(session_id).is_ok()
}

/// Ensure a session id is a single, ordinary path component (no separators, no
/// `.`/`..`, non-empty), so it cannot escape the sessions root. Session ids come
/// from external hook payloads, so this guards against path traversal.
fn validate_session_id(session_id: &str) -> Result<(), StoreError> {
    use std::path::Component;
    let mut components = Path::new(session_id).components();
    match (components.next(), components.next()) {
        // Exactly one normal component whose text matches the input verbatim.
        (Some(Component::Normal(only)), None) if only == session_id => Ok(()),
        _ => Err(StoreError::InvalidSessionId(session_id.to_string())),
    }
}

/// Parse a JSONL event file, skipping blank and unparseable lines.
fn read_events_file(events_path: &Path) -> Result<SessionRead, StoreError> {
    let (events, skipped_lines) = read_jsonl(events_path)?;
    Ok(SessionRead {
        events,
        skipped_lines,
    })
}

/// Shared JSONL walker: parse each non-empty line as `T`, counting (not
/// hiding) unparseable lines. A missing file reads as empty — every JSONL log
/// in the store shares this open/skip/count policy.
fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<(Vec<T>, usize), StoreError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(StoreError::io(path, e)),
    };

    let mut items = Vec::new();
    let mut skipped_lines = 0;
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|e| StoreError::io(path, e))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<T>(&line) {
            Ok(item) => items.push(item),
            Err(_) => skipped_lines += 1,
        }
    }
    Ok((items, skipped_lines))
}

/// Single-writer, append-only handle over one session's `events.jsonl`.
///
/// Holds the open file in append mode; each [`SessionWriter::append`] writes one
/// JSONL line and flushes. Only one writer per session should exist at a time.
#[derive(Debug)]
pub struct SessionWriter {
    events_path: PathBuf,
    file: File,
    raw_path: PathBuf,
    raw_file: File,
}

impl SessionWriter {
    /// Append one event as a single JSONL line and flush it to the OS.
    pub fn append(&mut self, event: &AgentEvent) -> Result<(), StoreError> {
        // Compact form guarantees a single line per event.
        let mut line = serde_json::to_string(event)?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .map_err(|e| StoreError::io(&self.events_path, e))?;
        self.file
            .flush()
            .map_err(|e| StoreError::io(&self.events_path, e))?;
        Ok(())
    }

    /// Append one raw source record as a single JSONL line and flush it. The
    /// verbatim payload lives in [`RawRecord::raw`]; serde escaping keeps it on
    /// one physical line even if the original payload spanned several.
    pub fn append_raw(&mut self, record: &RawRecord) -> Result<(), StoreError> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        self.raw_file
            .write_all(line.as_bytes())
            .map_err(|e| StoreError::io(&self.raw_path, e))?;
        self.raw_file
            .flush()
            .map_err(|e| StoreError::io(&self.raw_path, e))?;
        Ok(())
    }

    /// Path to the event log this writer appends to.
    pub fn events_path(&self) -> &Path {
        &self.events_path
    }

    /// Path to the raw source log this writer appends to.
    pub fn raw_path(&self) -> &Path {
        &self.raw_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Attribution, EventKind, Source, CONFIDENCE_CERTAIN};
    use serde_json::json;
    use tempfile::TempDir;

    const CREATED_TS: i64 = 1_700_000_000_000;

    fn event(session: &str, ts: i64, kind: EventKind) -> AgentEvent {
        AgentEvent::new(
            ts,
            session,
            Source::Hooks,
            kind,
            Attribution::Direct,
            CONFIDENCE_CERTAIN,
            json!({"tool_name": "Read"}),
        )
    }

    #[test]
    fn open_creates_session_dir_and_meta() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        let writer = store.open("sess-a", CREATED_TS).unwrap();

        let dir = tmp.path().join("sess-a");
        assert!(dir.is_dir());
        assert!(dir.join(META_FILE).is_file());
        assert_eq!(writer.events_path(), dir.join(EVENTS_FILE));

        let meta = store.read_meta("sess-a").unwrap();
        assert_eq!(
            meta,
            SessionMeta {
                v: SCHEMA_VERSION,
                session_id: "sess-a".to_string(),
                created_ts: CREATED_TS,
            }
        );
    }

    #[test]
    fn append_then_read_returns_events_in_order() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let mut writer = store.open("sess-b", CREATED_TS).unwrap();

        let e1 = event("sess-b", 1, EventKind::SessionStart);
        let e2 = event("sess-b", 2, EventKind::ToolCall);
        writer.append(&e1).unwrap();
        writer.append(&e2).unwrap();

        let read = store.read("sess-b").unwrap();
        assert_eq!(read.skipped_lines, 0);
        assert_eq!(read.events, vec![e1, e2]);
    }

    #[test]
    fn read_skips_corrupted_line_without_losing_subsequent_events() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let mut writer = store.open("sess-c", CREATED_TS).unwrap();

        let good1 = event("sess-c", 1, EventKind::Prompt);
        writer.append(&good1).unwrap();

        // Corrupt one line in the middle by writing raw garbage.
        {
            let mut raw = OpenOptions::new()
                .append(true)
                .open(writer.events_path())
                .unwrap();
            raw.write_all(b"{ this is not valid json\n").unwrap();
        }

        let good2 = event("sess-c", 2, EventKind::Stop);
        writer.append(&good2).unwrap();

        let read = store.read("sess-c").unwrap();
        assert_eq!(read.skipped_lines, 1);
        assert_eq!(read.events, vec![good1, good2]);
    }

    #[test]
    fn read_ignores_blank_lines() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let mut writer = store.open("sess-d", CREATED_TS).unwrap();
        let e = event("sess-d", 1, EventKind::ToolCall);
        writer.append(&e).unwrap();
        {
            let mut raw = OpenOptions::new()
                .append(true)
                .open(writer.events_path())
                .unwrap();
            raw.write_all(b"\n   \n").unwrap();
        }

        let read = store.read("sess-d").unwrap();
        assert_eq!(read.skipped_lines, 0);
        assert_eq!(read.events, vec![e]);
    }

    #[test]
    fn read_missing_session_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let read = store.read("nonexistent").unwrap();
        assert!(read.events.is_empty());
        assert_eq!(read.skipped_lines, 0);
    }

    #[test]
    fn rejects_path_traversal_session_ids() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        for bad in ["../escape", "a/b", "..", ".", "", "/abs", "nested/../x"] {
            let err = store.open(bad, CREATED_TS).unwrap_err();
            assert!(
                matches!(err, StoreError::InvalidSessionId(_)),
                "expected rejection for {bad:?}, got {err:?}"
            );
            assert!(store.read(bad).is_err());
            assert!(store.read_meta(bad).is_err());
        }
    }

    #[test]
    fn accepts_uuid_like_session_ids() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        assert!(store
            .open("9f8c1e2a-3b4d-4e5f-8a9b-0c1d2e3f4a5b", CREATED_TS)
            .is_ok());
    }

    #[test]
    fn append_raw_then_read_raw_round_trips_verbatim() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let mut writer = store.open("sess-raw", CREATED_TS).unwrap();

        // A payload with embedded newlines must survive as one JSONL line.
        let rec = RawRecord {
            v: SCHEMA_VERSION,
            ts: CREATED_TS,
            session: "sess-raw".to_string(),
            raw_ref: "raw-0".to_string(),
            raw: "{\"a\":1,\n\"b\":\"x\\ny\"}".to_string(),
        };
        writer.append_raw(&rec).unwrap();

        let back = store.read_raw("sess-raw").unwrap();
        assert_eq!(back.skipped_lines, 0);
        assert_eq!(back.records, vec![rec]);
    }

    #[test]
    fn read_raw_is_empty_for_new_session() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let read = store.read_raw("nope").unwrap();
        assert!(read.records.is_empty());
        assert_eq!(read.skipped_lines, 0);
    }

    #[test]
    fn read_raw_counts_corrupted_lines_instead_of_hiding_them() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let mut writer = store.open("sess-raw-bad", CREATED_TS).unwrap();
        let rec = RawRecord {
            v: SCHEMA_VERSION,
            ts: CREATED_TS,
            session: "sess-raw-bad".to_string(),
            raw_ref: "raw-0".to_string(),
            raw: "{}".to_string(),
        };
        writer.append_raw(&rec).unwrap();
        {
            let mut raw = OpenOptions::new()
                .append(true)
                .open(writer.raw_path())
                .unwrap();
            raw.write_all(b"not a raw record\n").unwrap();
        }

        let read = store.read_raw("sess-raw-bad").unwrap();
        assert_eq!(read.records, vec![rec]);
        assert_eq!(read.skipped_lines, 1);
    }

    fn sample_usage(session: &str) -> SessionUsage {
        const LINE: &str = r#"{"type":"assistant","message":{"id":"m","model":"claude-fable-5","content":[],"usage":{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":30,"cache_read_input_tokens":40}}}"#;
        crate::transcript::aggregate_transcript_usage(LINE, session)
    }

    #[test]
    fn write_usage_then_read_usage_round_trips() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let usage = sample_usage("sess-usage");

        store.write_usage(&usage).unwrap();

        let back = store.read_usage("sess-usage").unwrap();
        assert_eq!(back, Some(usage));
        // The sidecar exists and the temp file was renamed away.
        let dir = tmp.path().join("sess-usage");
        assert!(dir.join(USAGE_FILE).is_file());
        assert!(!dir.join(USAGE_TMP_FILE).exists());
    }

    #[test]
    fn write_usage_overwrites_previous_sidecar() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        store.write_usage(&sample_usage("sess-ow")).unwrap();
        // A second write of the same derived data must not accumulate.
        store.write_usage(&sample_usage("sess-ow")).unwrap();

        let back = store.read_usage("sess-ow").unwrap().unwrap();
        assert_eq!(back.per_model.len(), 1);
        assert_eq!(back.per_model[0].input_tokens, 10);
    }

    #[test]
    fn read_usage_is_none_when_absent() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        // Absent file == no claim, never an error and never zeroed totals.
        assert_eq!(store.read_usage("never-written").unwrap(), None);
    }

    #[test]
    fn read_usage_degrades_to_none_on_corrupted_file() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let dir = tmp.path().join("sess-bad-usage");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(USAGE_FILE), b"{ not valid json").unwrap();

        // Corrupted sidecar degrades gracefully rather than erroring the read.
        assert_eq!(store.read_usage("sess-bad-usage").unwrap(), None);
    }

    #[test]
    fn usage_read_write_reject_invalid_session_ids() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let mut bad = sample_usage("../escape");
        bad.session = "../escape".to_string();
        assert!(store.write_usage(&bad).is_err());
        assert!(store.read_usage("../escape").is_err());
    }

    #[test]
    fn list_sessions_is_empty_before_any_recording() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path().join("does-not-exist-yet"));
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn list_sessions_returns_opened_sessions_sorted() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        store.open("sess-c", CREATED_TS).unwrap();
        store.open("sess-a", CREATED_TS).unwrap();
        store.open("sess-b", CREATED_TS).unwrap();

        assert_eq!(
            store.list_sessions().unwrap(),
            vec!["sess-a", "sess-b", "sess-c"]
        );
    }

    #[test]
    fn list_sessions_ignores_stray_files() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        store.open("real-session", CREATED_TS).unwrap();
        fs::write(tmp.path().join("stray.txt"), b"not a session").unwrap();

        assert_eq!(store.list_sessions().unwrap(), vec!["real-session"]);
    }

    #[test]
    fn reopen_preserves_meta_and_appends() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());

        let mut w1 = store.open("sess-e", CREATED_TS).unwrap();
        w1.append(&event("sess-e", 1, EventKind::SessionStart))
            .unwrap();
        drop(w1);

        // Re-open with a different created_ts: original meta must be preserved.
        let mut w2 = store.open("sess-e", CREATED_TS + 999).unwrap();
        w2.append(&event("sess-e", 2, EventKind::Stop)).unwrap();

        let meta = store.read_meta("sess-e").unwrap();
        assert_eq!(meta.created_ts, CREATED_TS);

        let read = store.read("sess-e").unwrap();
        assert_eq!(read.events.len(), 2);
    }
}
