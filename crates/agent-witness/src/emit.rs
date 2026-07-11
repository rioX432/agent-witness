//! `agent-witness emit`: the hooks bridge.
//!
//! Invoked by Claude Code as a hook command. It reads one hook payload on stdin
//! (the canonical record, ADR-0001) and forwards it to the `watch` daemon over a
//! unix socket. If no daemon is listening — or the daemon fails to confirm
//! persistence — it falls back to writing directly to the session store, so
//! recording works with **no daemon required** (Zero-friction adoption).
//!
//! Protocol: one `emit` invocation == one hook == one payload == one
//! connection. The whole payload is sent, the write side is shut down, and the
//! daemon replies with a single [`ACK_BYTE`] **after** it has persisted the
//! record. No ack (connection refused, daemon died mid-send, store error on the
//! daemon side) triggers the direct-store fallback. Delivery is therefore
//! at-least-once: a record can be duplicated in the rare ack-lost case, but is
//! never silently lost — the right trade-off for an audit log.

use std::path::Path;
use std::time::Duration;

use agent_witness_core::{Receiver, SessionStore, SystemClock};
use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Byte the daemon sends once the payload is durably persisted (ASCII ACK).
pub const ACK_BYTE: u8 = 0x06;

/// How long `emit` waits for the daemon's ack before falling back. Claude Code
/// applies its own hook timeout on top; this keeps a wedged daemon from
/// stalling the agent for that long.
const ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// Which path a payload took — surfaced for tests and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitOutcome {
    /// Forwarded to a running `watch` daemon, which acked persistence.
    Forwarded,
    /// No daemon ack (absent, crashed, or store failure); written straight to
    /// the session store by this process.
    Fallback,
}

/// Forward `payload` to the daemon, or fall back to the store when the daemon
/// does not confirm persistence. Every failure mode after `connect` (send
/// error, missing/timed-out ack) also falls back, so a hook record is never
/// lost as long as the local store is writable.
///
/// `transcript_enabled` gates the best-effort transcript adapter on the fallback
/// path only; when the daemon acks, the `watch` server owns transcript ingest.
pub async fn run_emit(
    socket: &Path,
    sessions_root: &Path,
    payload: String,
    transcript_enabled: bool,
) -> Result<EmitOutcome> {
    match forward(socket, &payload).await {
        Ok(()) => Ok(EmitOutcome::Forwarded),
        Err(_) => {
            fallback(sessions_root, &payload, transcript_enabled)?;
            Ok(EmitOutcome::Fallback)
        }
    }
}

/// Send the payload and wait for the persistence ack. Any error means the
/// daemon did not confirm the record.
async fn forward(socket: &Path, payload: &str) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(socket).await?;
    stream.write_all(payload.as_bytes()).await?;
    // Half-close: signals end-of-message so the server's read-to-EOF completes.
    stream.shutdown().await?;

    let mut ack = [0u8; 1];
    let read = tokio::time::timeout(ACK_TIMEOUT, stream.read(&mut ack))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "ack timeout"))??;
    if read == 1 && ack[0] == ACK_BYTE {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "daemon closed without acking persistence",
        ))
    }
}

/// Write the payload directly to the store (no daemon). Uses the wall clock at
/// the edge; the normalizer it drives stays clock-free. After the canonical hook
/// record lands, best-effort transcript ingest runs (on `Stop` /
/// `SessionEnd`); its failures are isolated and never fail this hook.
fn fallback(sessions_root: &Path, payload: &str, transcript_enabled: bool) -> Result<()> {
    let mut receiver = Receiver::new(SessionStore::new(sessions_root));
    receiver.ingest(payload, &SystemClock)?;
    crate::transcript::ingest_on_stop(&mut receiver, payload, &SystemClock, transcript_enabled);
    Ok(())
}
