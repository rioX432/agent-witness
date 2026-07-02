//! `agent-witness watch`: the unix socket server.
//!
//! Accepts connections from the `emit` bridge, reads one payload per connection
//! (to EOF), hands it to the core [`Receiver`] (normalize + persist raw and
//! normalized records, ADR-0001), and only then replies with the persistence
//! ack ([`crate::emit::ACK_BYTE`]). If ingest fails, no ack is sent, so the
//! `emit` side falls back to writing the record itself — a store error here
//! never silently loses a record.
//!
//! v0.1 handles connections sequentially: within a single Claude Code session
//! hooks fire one at a time, and a single writer per session is the store's
//! invariant; ingest is synchronous file I/O held inline (an accepted v0.1
//! trade-off — revisit with spawn_blocking if concurrent accepts land in
//! v0.2). A malformed payload or a single store error is logged and the daemon
//! keeps running — one bad hook must not take down recording.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use agent_witness_core::{Clock, Receiver, SessionStore};
use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::emit::ACK_BYTE;

/// Run the socket server until `shutdown` resolves.
///
/// Creates the socket's parent directory, removes any stale socket file, binds,
/// then serves. On shutdown the socket file is removed (best effort). `clock` is
/// injected so tests can pin timestamps.
pub async fn run_watch<F>(
    socket: &Path,
    sessions_root: &Path,
    clock: Arc<dyn Clock + Send + Sync>,
    transcript_enabled: bool,
    shutdown: F,
) -> Result<()>
where
    F: Future<Output = ()>,
{
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating socket directory {}", parent.display()))?;
    }
    remove_stale_socket(socket)?;

    let listener = UnixListener::bind(socket)
        .with_context(|| format!("binding unix socket {}", socket.display()))?;
    let mut receiver = Receiver::new(SessionStore::new(sessions_root));

    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _addr)) => {
                        handle_connection(stream, &mut receiver, clock.as_ref(), transcript_enabled).await;
                    }
                    Err(e) => {
                        // Transient accept error: log and keep serving.
                        eprintln!("agent-witness watch: accept error: {e}");
                    }
                }
            }
        }
    }

    // Best-effort cleanup so a future bind() is not blocked by a stale file.
    let _ = std::fs::remove_file(socket);
    Ok(())
}

/// Serve one `emit` connection: read the payload to EOF, persist it, then ack.
///
/// The payload is read as raw bytes and converted lossily: even non-UTF8 input
/// still reaches the receiver and is preserved (best-effort) as a raw record +
/// error event instead of vanishing with only a stderr line. The ack is written
/// only after `ingest` returned Ok — i.e. after both records were flushed — so
/// a no-ack close tells `emit` to fall back.
async fn handle_connection(
    mut stream: UnixStream,
    receiver: &mut Receiver,
    // `+ Sync` keeps this future Send so run_watch can run under tokio::spawn.
    clock: &(dyn Clock + Sync),
    transcript_enabled: bool,
) {
    let mut bytes = Vec::new();
    if let Err(e) = stream.read_to_end(&mut bytes).await {
        eprintln!("agent-witness watch: read error: {e}");
        return; // no ack -> emit falls back with the payload it still holds
    }
    let payload = String::from_utf8_lossy(&bytes);

    // Hold the (non-async) ingest inline; no await while the receiver's
    // per-session state is mutated.
    match receiver.ingest(&payload, clock) {
        Ok(_) => {
            // Persisted: confirm to the bridge. An ack write failure is fine —
            // emit falls back and the record is duplicated, never lost.
            if let Err(e) = stream.write_all(&[ACK_BYTE]).await {
                eprintln!("agent-witness watch: ack write error: {e}");
            }
            // Best-effort transcript supplement after the canonical hook is
            // durable; isolated so it never affects the ack or recording.
            crate::transcript::ingest_on_stop(receiver, &payload, clock, transcript_enabled);
        }
        Err(e) => {
            eprintln!("agent-witness watch: ingest error: {e}");
            // The failed seq left a gap and emit's fallback may write records
            // directly now; drop cached sequences so the next ingest re-seeds
            // from disk and cannot reissue a raw_ref.
            receiver.reset_seq_cache();
        }
    }
}

/// Remove a leftover socket file (e.g. from a crashed daemon) so `bind` can
/// recreate it. Absence is not an error.
fn remove_stale_socket(socket: &Path) -> Result<()> {
    match std::fs::remove_file(socket) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing stale socket {}", socket.display())),
    }
}
