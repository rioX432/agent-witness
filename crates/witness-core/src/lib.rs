//! Event model, session store, and adapters for agent-witness.
//!
//! - [`event`]: the normalized [`AgentEvent`] record (one JSONL line each).
//! - [`store`]: append-only per-session JSONL store.
//! - [`clock`]: injectable time source (no wall-clock in pure paths).
//! - [`hooks`]: pure normalizer from Claude Code hook payloads to [`AgentEvent`].
//! - [`receiver`]: adapter that persists raw + normalized records together.
//! - [`transcript`]: best-effort, versioned secondary adapter over the session
//!   transcript file (supplements context hooks do not surface).
//!
//! See CLAUDE.md and `docs/adr/` for the design; ADR-0001 makes hooks the
//! canonical source and ADR-0002 governs attribution honesty.

pub mod clock;
pub mod event;
pub mod hooks;
pub mod receiver;
pub mod store;
pub mod transcript;

pub use clock::{Clock, FixedClock, SystemClock};
pub use event::{AgentEvent, Attribution, EventKind, Source, CONFIDENCE_CERTAIN, SCHEMA_VERSION};
pub use hooks::{normalize, NormalizeError, UNKNOWN_SESSION};
pub use receiver::{IngestError, Ingested, Receiver, TranscriptIngested};
pub use store::{
    RawRead, RawRecord, SessionMeta, SessionRead, SessionStore, SessionWriter, StoreError,
};
pub use transcript::{
    parse_transcript, TranscriptRead, TranscriptStats,
    ADAPTER_VERSION as TRANSCRIPT_ADAPTER_VERSION, TRANSCRIPT_RAW_REF_PREFIX,
};
