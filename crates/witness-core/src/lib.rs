//! Event model, session store, and adapters for agent-witness.
//!
//! - [`event`]: the normalized [`AgentEvent`] record (one JSONL line each).
//! - [`store`]: append-only per-session JSONL store.
//! - [`clock`]: injectable time source (no wall-clock in pure paths).
//!
//! See CLAUDE.md and `docs/adr/` for the design; ADR-0002 governs attribution
//! honesty, which every event carries.

pub mod clock;
pub mod event;
pub mod store;

pub use clock::{Clock, FixedClock, SystemClock};
pub use event::{AgentEvent, Attribution, EventKind, Source, CONFIDENCE_CERTAIN, SCHEMA_VERSION};
pub use store::{SessionMeta, SessionRead, SessionStore, SessionWriter, StoreError};
