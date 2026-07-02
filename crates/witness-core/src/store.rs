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

/// File name for the per-session event log.
const EVENTS_FILE: &str = "events.jsonl";
/// File name for the per-session metadata.
const META_FILE: &str = "meta.json";

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

        Ok(SessionWriter { events_path, file })
    }

    /// Read and parse a session's event log, skipping corrupted lines so one
    /// bad line never blocks the rest. Returns an empty read if the log does
    /// not exist yet.
    pub fn read(&self, session_id: &str) -> Result<SessionRead, StoreError> {
        validate_session_id(session_id)?;
        let events_path = self.session_dir(session_id).join(EVENTS_FILE);
        read_events_file(&events_path)
    }

    /// Load a session's metadata.
    pub fn read_meta(&self, session_id: &str) -> Result<SessionMeta, StoreError> {
        validate_session_id(session_id)?;
        let meta_path = self.session_dir(session_id).join(META_FILE);
        let bytes = fs::read(&meta_path).map_err(|e| StoreError::io(&meta_path, e))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
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
    let file = match File::open(events_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SessionRead {
                events: Vec::new(),
                skipped_lines: 0,
            });
        }
        Err(e) => return Err(StoreError::io(events_path, e)),
    };

    let reader = BufReader::new(file);
    let mut events = Vec::new();
    let mut skipped_lines = 0;
    for line in reader.lines() {
        let line = line.map_err(|e| StoreError::io(events_path, e))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<AgentEvent>(&line) {
            Ok(ev) => events.push(ev),
            Err(_) => skipped_lines += 1,
        }
    }
    Ok(SessionRead {
        events,
        skipped_lines,
    })
}

/// Single-writer, append-only handle over one session's `events.jsonl`.
///
/// Holds the open file in append mode; each [`SessionWriter::append`] writes one
/// JSONL line and flushes. Only one writer per session should exist at a time.
#[derive(Debug)]
pub struct SessionWriter {
    events_path: PathBuf,
    file: File,
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

    /// Path to the event log this writer appends to.
    pub fn events_path(&self) -> &Path {
        &self.events_path
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
