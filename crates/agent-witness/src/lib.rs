//! Binary-side library for `agent-witness`.
//!
//! Split from `main.rs` so the `emit` bridge and `watch` server are directly
//! integration-testable (see `tests/`). The core event model, normalizer, and
//! store live in `agent-witness-core`.

pub mod emit;
pub mod init;
pub mod paths;
pub mod transcript;
pub mod watch;
