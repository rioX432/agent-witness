//! Transcript bridge: trigger the best-effort transcript adapter at the edge.
//!
//! Hooks are canonical (ADR-0001). Once a turn ends (`Stop`) or the session
//! terminates (`SessionEnd`), the transcript file is complete, so this reads
//! exactly the `transcript_path` the hook payload carries — never a glob
//! (CLAUDE.md) — and hands its contents to the core [`Receiver`] as
//! supplementary `Observed` events.
//!
//! Isolation is the contract (issue #4): the transcript is a secondary source
//! and DEFAULT ON, but any failure here — disabled, wrong hook, missing path,
//! unreadable file, store error — is logged and swallowed. It must never break
//! or corrupt the canonical hooks-based recording.

use agent_witness_core::{hooks, Clock, Receiver};
use serde_json::Value;

/// Transcript ingestion is on unless explicitly disabled (`--no-transcript`).
pub const TRANSCRIPT_ENABLED_DEFAULT: bool = true;

/// If enabled and `payload` is a `Stop` or `SessionEnd` hook carrying a
/// `transcript_path`, read that file and ingest supplementary events. Errors
/// are isolated: logged to stderr, never propagated — canonical recording has
/// already completed by the time this runs.
pub fn ingest_on_stop(receiver: &mut Receiver, payload: &str, clock: &dyn Clock, enabled: bool) {
    if !enabled {
        return;
    }
    let value: Value = match serde_json::from_str(payload.trim()) {
        Ok(v) => v,
        Err(_) => return, // not JSON: the receiver already recorded it as an error event
    };
    // Only act at end-of-turn or session end, when the transcript is complete.
    let event_name = hooks::hook_event_name_of(&value);
    if event_name != Some(hooks::HOOK_STOP) && event_name != Some(hooks::HOOK_SESSION_END) {
        return;
    }
    let session = match hooks::session_id_of(&value) {
        Some(s) => s,
        None => return,
    };
    let path = match hooks::transcript_path_of(&value) {
        Some(p) => p,
        None => return,
    };

    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => {
            eprintln!("agent-witness: transcript read skipped ({path}): {e}");
            return;
        }
    };
    match receiver.ingest_transcript(session, &content, clock) {
        Ok(out) => {
            if out.appended > 0
                || out.stats.skipped_unparseable > 0
                || out.stats.skipped_unrecognized > 0
            {
                eprintln!(
                    "agent-witness: transcript {session}: +{} event(s) \
                     (unparseable {}, unrecognized {}, duplicate {})",
                    out.appended,
                    out.stats.skipped_unparseable,
                    out.stats.skipped_unrecognized,
                    out.skipped_duplicates,
                );
            }
        }
        Err(e) => eprintln!("agent-witness: transcript ingest skipped ({session}): {e}"),
    }
}
