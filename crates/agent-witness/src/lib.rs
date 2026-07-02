//! Binary-side library for `agent-witness`.
//!
//! Split from `main.rs` so the `emit` bridge and `watch` server are directly
//! integration-testable (see `tests/`). The core event model, normalizer, and
//! store live in `agent-witness-core`.

pub mod emit;
pub mod init;
pub mod ls;
pub mod paths;
pub mod pick;
pub mod report;
pub mod timefmt;
pub mod timeline;
pub mod top;
pub mod transcript;
pub mod tui;
pub mod watch;
