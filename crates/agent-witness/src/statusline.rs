//! `agent-witness statusline`: ambient recording visibility inside Claude Code.
//!
//! Claude Code's `statusLine` setting runs ONE command, feeds it a JSON payload
//! on stdin (`session_id`, `cwd`, ...), and displays the first stdout line
//! (event-driven, ~300ms debounce). This module provides both halves:
//!
//! - **Runtime** ([`render`]): print a segment for the *current* session —
//!   `● witness 42ev` while events are landing, `○ witness not recording`
//!   when none have — so a silent hook failure is visible, not invisible.
//! - **Install** ([`run_statusline_install`], opt-in via `init --statusline`):
//!   edit `settings.json` under the same contract as the hooks edit
//!   (idempotent, backed up, never clobbering). Because only one statusLine
//!   command can exist, a foreign command is **wrapped**, not replaced: we
//!   store it verbatim inside our own command line as a base64url envelope
//!   (`--wrap-v1 <payload>`), run it at render time with the same stdin, and
//!   prepend its first output line to ours. `init --remove` decodes the
//!   envelope and restores the original exactly. No side-channel state: the
//!   original command's source of truth is the one setting Claude executes.
//!
//! Degradation is one-way honest: if the wrapped command fails or times out we
//! still render our segment; if our store lookup fails we still render theirs.
//! The statusline must never hang the UI, so the wrapped command gets a strict
//! timeout below Claude Code's debounce interval.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_witness_core::Clock;
use anyhow::{Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::{backup, init};

/// The bare statusline command installed when no statusLine exists.
const STATUSLINE_COMMAND: &str = "agent-witness statusline";
/// Flag carrying the wrap envelope. Versioned so a future format change can
/// migrate instead of guessing.
const WRAP_FLAG: &str = "--wrap-v1";
/// `installed_by` value expected inside a valid envelope.
const INSTALLED_BY: &str = "agent-witness";
/// Current envelope version.
const WRAP_VERSION: u32 = 1;
/// Timeout for the wrapped original command. Must stay below Claude Code's
/// ~300ms statusline debounce so a slow foreign command cannot stall the UI.
const WRAP_TIMEOUT_MS: u64 = 250;

/// Top-level settings key for the status line.
const STATUSLINE_KEY: &str = "statusLine";
/// Keys of the statusLine object.
const TYPE_KEY: &str = "type";
const COMMAND_KEY: &str = "command";
/// The only statusLine type we understand and install.
const COMMAND_TYPE: &str = "command";

/// Store file whose lines are the session's recorded events.
const EVENTS_FILE: &str = "events.jsonl";

/// Segment shown while the session's events are landing in the store.
const SEG_RECORDING: &str = "\u{25CF} witness"; // ●
/// Segment shown when nothing has been recorded for this session — a silent
/// hook failure must be visible (Core Value: honest observation).
const SEG_NOT_RECORDING: &str = "\u{25CB} witness not recording"; // ○
/// Separator between the wrapped original's segment and ours.
const SEG_SEPARATOR: &str = " | ";

/// The original command preserved inside our wrapper, plus provenance fields
/// that make "is this really our envelope?" checkable rather than guessed.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct WrapEnvelope {
    original_command: String,
    installed_by: String,
    version: u32,
}

/// What the statusline step of `init` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatuslineOutcome {
    /// No statusLine existed; ours was installed.
    Installed,
    /// A foreign command existed; it was wrapped (preserved verbatim inside
    /// our command) and keeps rendering ahead of our segment.
    Wrapped,
    /// Our command (bare or wrapping) was already in place.
    AlreadyInstalled,
    /// Our bare command was removed; no original to restore.
    Removed,
    /// Our wrapper was removed and the original command restored exactly.
    Restored,
    /// No statusLine of ours was present; nothing to remove.
    NothingToRemove,
    /// A statusLine we don't understand (non-command type, non-string command,
    /// or an undecodable wrapper) was left untouched.
    SkippedUnrecognized,
}

/// Result of the statusline install/remove step.
#[derive(Debug, Clone)]
pub struct StatuslineReport {
    /// Outcome of the run.
    pub outcome: StatuslineOutcome,
    /// Backup written before the settings file was rewritten, if any.
    pub backup: Option<PathBuf>,
}

/// Install (or, with `remove`, uninstall) the statusline in `settings_path`.
pub fn run_statusline_install(
    settings_path: &Path,
    remove: bool,
    clock: &dyn Clock,
) -> Result<StatuslineReport> {
    let loaded = init::load_settings(settings_path)?;

    if remove {
        let Some(mut root) = loaded else {
            return Ok(report(StatuslineOutcome::NothingToRemove, None));
        };
        let outcome = match classify(root.get(STATUSLINE_KEY)) {
            Existing::Absent | Existing::Foreign(_) => StatuslineOutcome::NothingToRemove,
            Existing::Unrecognized => StatuslineOutcome::SkippedUnrecognized,
            Existing::OursBare => {
                root.remove(STATUSLINE_KEY);
                StatuslineOutcome::Removed
            }
            Existing::OursWrapping(envelope) => {
                set_command(&mut root, &envelope.original_command)?;
                StatuslineOutcome::Restored
            }
        };
        if !matches!(
            outcome,
            StatuslineOutcome::Removed | StatuslineOutcome::Restored
        ) {
            return Ok(report(outcome, None));
        }
        let backup = backup_settings(settings_path, clock)?;
        init::write_settings(settings_path, &root)?;
        return Ok(report(outcome, backup));
    }

    let mut root = loaded.unwrap_or_default();
    let outcome = match classify(root.get(STATUSLINE_KEY)) {
        Existing::OursBare | Existing::OursWrapping(_) => {
            return Ok(report(StatuslineOutcome::AlreadyInstalled, None));
        }
        Existing::Unrecognized => {
            return Ok(report(StatuslineOutcome::SkippedUnrecognized, None));
        }
        Existing::Absent => {
            root.insert(
                STATUSLINE_KEY.to_string(),
                json!({ TYPE_KEY: COMMAND_TYPE, COMMAND_KEY: STATUSLINE_COMMAND }),
            );
            StatuslineOutcome::Installed
        }
        Existing::Foreign(original) => {
            set_command(&mut root, &wrapped_command(&original)?)?;
            StatuslineOutcome::Wrapped
        }
    };
    let backup = backup_settings(settings_path, clock)?;
    init::write_settings(settings_path, &root)?;
    Ok(report(outcome, backup))
}

/// Classification of the current statusLine value.
enum Existing {
    /// No statusLine key.
    Absent,
    /// Our bare command.
    OursBare,
    /// Our wrapper with a valid envelope.
    OursWrapping(WrapEnvelope),
    /// A `type: command` statusLine with some other command string (carried
    /// verbatim, ready to wrap).
    Foreign(String),
    /// Anything we don't understand — including a command that *looks* like
    /// our wrapper but whose envelope does not decode. Never touched.
    Unrecognized,
}

fn classify(value: Option<&Value>) -> Existing {
    let Some(value) = value else {
        return Existing::Absent;
    };
    let Some(command) = command_of(Some(value)) else {
        return Existing::Unrecognized;
    };
    if command == STATUSLINE_COMMAND {
        return Existing::OursBare;
    }
    if let Some(payload) = command
        .strip_prefix(STATUSLINE_COMMAND)
        .and_then(|rest| rest.trim_start().strip_prefix(WRAP_FLAG))
    {
        return match decode_wrap(payload.trim()) {
            Some(envelope) => Existing::OursWrapping(envelope),
            None => Existing::Unrecognized,
        };
    }
    if command.starts_with(STATUSLINE_COMMAND) {
        // Ours-shaped but with flags we don't recognize: leave it alone.
        return Existing::Unrecognized;
    }
    Existing::Foreign(command.to_string())
}

/// The command string of a `{type: "command", command: "..."}` statusLine, if
/// that is what `value` is.
fn command_of(value: Option<&Value>) -> Option<&str> {
    let obj = value?.as_object()?;
    if obj.get(TYPE_KEY).and_then(Value::as_str) != Some(COMMAND_TYPE) {
        return None;
    }
    obj.get(COMMAND_KEY).and_then(Value::as_str)
}

/// Replace the statusLine's command in place, preserving its other fields
/// (e.g. `padding`). Errors if the current shape is not an object — callers
/// only reach this after [`classify`] proved it is.
fn set_command(root: &mut Map<String, Value>, command: &str) -> Result<()> {
    let obj = root
        .get_mut(STATUSLINE_KEY)
        .and_then(Value::as_object_mut)
        .context("statusLine is unexpectedly not an object")?;
    obj.insert(COMMAND_KEY.to_string(), json!(command));
    obj.insert(TYPE_KEY.to_string(), json!(COMMAND_TYPE));
    Ok(())
}

/// Our wrapper command embedding `original` as a versioned envelope.
fn wrapped_command(original: &str) -> Result<String> {
    let envelope = WrapEnvelope {
        original_command: original.to_string(),
        installed_by: INSTALLED_BY.to_string(),
        version: WRAP_VERSION,
    };
    let payload =
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&envelope).context("cannot encode envelope")?);
    Ok(format!("{STATUSLINE_COMMAND} {WRAP_FLAG} {payload}"))
}

/// Decode and validate a wrap payload. `None` for anything not verifiably ours.
fn decode_wrap(payload: &str) -> Option<WrapEnvelope> {
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let envelope: WrapEnvelope = serde_json::from_slice(&bytes).ok()?;
    (envelope.installed_by == INSTALLED_BY && envelope.version == WRAP_VERSION).then_some(envelope)
}

fn report(outcome: StatuslineOutcome, backup: Option<PathBuf>) -> StatuslineReport {
    StatuslineReport { outcome, backup }
}

/// Back up the settings file (if it exists) before rewriting it.
fn backup_settings(path: &Path, clock: &dyn Clock) -> Result<Option<PathBuf>> {
    match std::fs::read(path) {
        Ok(raw) => Ok(Some(backup::write_backup(path, &raw, clock.now_ms())?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::anyhow!(
            "cannot read {} for backup: {e}",
            path.display()
        )),
    }
}

// ---------------------------------------------------------------------------
// Runtime: render the segment for the current session.
// ---------------------------------------------------------------------------

/// Render the full statusline for one invocation: the wrapped original's first
/// line (if any) followed by our witness segment. Never fails — every error
/// degrades to the best line we can still render honestly.
pub async fn render(stdin_json: &str, sessions_root: &Path, wrap_payload: Option<&str>) -> String {
    let ours = witness_segment(stdin_json, sessions_root);
    let Some(original) = wrap_payload.and_then(decode_wrap) else {
        return ours;
    };
    // Nesting guard: never execute ourselves from inside ourselves.
    if original.original_command.starts_with(STATUSLINE_COMMAND) {
        return ours;
    }
    match run_original(&original.original_command, stdin_json).await {
        Some(line) if !line.is_empty() => format!("{line}{SEG_SEPARATOR}{ours}"),
        _ => ours,
    }
}

/// Our segment for the session named in the statusline stdin JSON.
///
/// Reads only this session's `events.jsonl` and counts lines — no JSON parse,
/// no store scan — so the ~3/sec statusline cadence stays cheap. A missing or
/// empty file renders as *not recording*: silence must be visible.
fn witness_segment(stdin_json: &str, sessions_root: &Path) -> String {
    let session_id = serde_json::from_str::<Value>(stdin_json)
        .ok()
        .and_then(|v| {
            v.get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let Some(session_id) = session_id else {
        return SEG_NOT_RECORDING.to_string();
    };
    // The id lands in a path join; refuse anything that could escape the store.
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains("..")
    {
        return SEG_NOT_RECORDING.to_string();
    }
    let events = sessions_root.join(&session_id).join(EVENTS_FILE);
    match std::fs::read(&events) {
        Ok(bytes) if !bytes.is_empty() => {
            let count = bytes.iter().filter(|b| **b == b'\n').count();
            format!("{SEG_RECORDING} {count}ev")
        }
        _ => SEG_NOT_RECORDING.to_string(),
    }
}

/// Run the wrapped original command the way Claude Code would (`sh -c`), feed
/// it the same stdin JSON, and return its first stdout line. `None` on any
/// failure or on timeout — the caller degrades to our segment alone.
async fn run_original(command: &str, stdin_json: &str) -> Option<String> {
    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        // Feed the payload; a command that never reads stdin must not block us.
        let _ = stdin.write_all(stdin_json.as_bytes()).await;
        drop(stdin);
    }
    let output = tokio::time::timeout(
        Duration::from_millis(WRAP_TIMEOUT_MS),
        child.wait_with_output(),
    )
    .await
    .ok()?
    .ok()?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    Some(
        stdout
            .lines()
            .next()
            .unwrap_or_default()
            .trim_end()
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_witness_core::FixedClock;

    const TS: i64 = 1_700_000_000_000;

    fn clock() -> FixedClock {
        FixedClock(TS)
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
    }

    fn settings_path(tmp: &tempfile::TempDir) -> PathBuf {
        tmp.path().join(".claude").join("settings.json")
    }

    #[test]
    fn wrap_envelope_round_trips() {
        let cmd = wrapped_command("~/bin/statusline.sh --fancy 'a b'").unwrap();
        let payload = cmd
            .strip_prefix(STATUSLINE_COMMAND)
            .unwrap()
            .trim_start()
            .strip_prefix(WRAP_FLAG)
            .unwrap()
            .trim();
        let envelope = decode_wrap(payload).expect("round trip");
        assert_eq!(
            envelope.original_command,
            "~/bin/statusline.sh --fancy 'a b'"
        );
    }

    #[test]
    fn decode_rejects_garbage_and_foreign_envelopes() {
        assert!(decode_wrap("not base64!!!").is_none());
        assert!(decode_wrap(&URL_SAFE_NO_PAD.encode(b"[1,2,3]")).is_none());
        let foreign = serde_json::to_vec(&WrapEnvelope {
            original_command: "x".into(),
            installed_by: "someone-else".into(),
            version: WRAP_VERSION,
        })
        .unwrap();
        assert!(decode_wrap(&URL_SAFE_NO_PAD.encode(&foreign)).is_none());
    }

    #[test]
    fn install_into_empty_settings_sets_bare_command_and_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = settings_path(&tmp);

        let r = run_statusline_install(&path, false, &clock()).unwrap();
        assert_eq!(r.outcome, StatuslineOutcome::Installed);
        let root = read_json(&path);
        assert_eq!(root[STATUSLINE_KEY][COMMAND_KEY], json!(STATUSLINE_COMMAND));
        assert_eq!(root[STATUSLINE_KEY][TYPE_KEY], json!(COMMAND_TYPE));

        let again = run_statusline_install(&path, false, &clock()).unwrap();
        assert_eq!(again.outcome, StatuslineOutcome::AlreadyInstalled);
    }

    #[test]
    fn foreign_command_is_wrapped_and_restored_exactly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = settings_path(&tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "~/bin/my-status.sh --theme 'solar dark'";
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "model": "opus",
                STATUSLINE_KEY: { TYPE_KEY: COMMAND_TYPE, COMMAND_KEY: original, "padding": 0 }
            }))
            .unwrap(),
        )
        .unwrap();

        let r = run_statusline_install(&path, false, &clock()).unwrap();
        assert_eq!(r.outcome, StatuslineOutcome::Wrapped);
        assert!(r.backup.is_some(), "existing settings must be backed up");
        let root = read_json(&path);
        let command = root[STATUSLINE_KEY][COMMAND_KEY].as_str().unwrap();
        assert!(command.starts_with(STATUSLINE_COMMAND));
        // Other statusLine fields and unrelated settings survive.
        assert_eq!(root[STATUSLINE_KEY]["padding"], json!(0));
        assert_eq!(root["model"], json!("opus"));

        // Idempotent: a second install run recognizes its own wrapper.
        let again = run_statusline_install(&path, false, &clock()).unwrap();
        assert_eq!(again.outcome, StatuslineOutcome::AlreadyInstalled);

        // Remove restores the original exactly, keeping the extra fields.
        let removed = run_statusline_install(&path, true, &clock()).unwrap();
        assert_eq!(removed.outcome, StatuslineOutcome::Restored);
        let root = read_json(&path);
        assert_eq!(root[STATUSLINE_KEY][COMMAND_KEY], json!(original));
        assert_eq!(root[STATUSLINE_KEY]["padding"], json!(0));
    }

    #[test]
    fn remove_of_bare_install_drops_the_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = settings_path(&tmp);
        run_statusline_install(&path, false, &clock()).unwrap();

        let r = run_statusline_install(&path, true, &clock()).unwrap();
        assert_eq!(r.outcome, StatuslineOutcome::Removed);
        assert!(read_json(&path).get(STATUSLINE_KEY).is_none());
    }

    #[test]
    fn remove_leaves_foreign_statusline_untouched() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = settings_path(&tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let settings = json!({
            STATUSLINE_KEY: { TYPE_KEY: COMMAND_TYPE, COMMAND_KEY: "their-status" }
        });
        std::fs::write(&path, serde_json::to_string(&settings).unwrap()).unwrap();

        let r = run_statusline_install(&path, true, &clock()).unwrap();
        assert_eq!(r.outcome, StatuslineOutcome::NothingToRemove);
        assert_eq!(read_json(&path), settings);
    }

    #[test]
    fn unrecognized_shapes_are_never_touched() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = settings_path(&tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A non-command type and an ours-shaped command with a broken payload.
        for weird in [
            json!({ STATUSLINE_KEY: { TYPE_KEY: "plugin", "name": "x" } }),
            json!({ STATUSLINE_KEY: {
                TYPE_KEY: COMMAND_TYPE,
                COMMAND_KEY: format!("{STATUSLINE_COMMAND} {WRAP_FLAG} broken!!payload")
            } }),
        ] {
            std::fs::write(&path, serde_json::to_string(&weird).unwrap()).unwrap();
            for remove in [false, true] {
                let r = run_statusline_install(&path, remove, &clock()).unwrap();
                assert_eq!(r.outcome, StatuslineOutcome::SkippedUnrecognized);
                assert_eq!(read_json(&path), weird);
            }
        }
    }

    #[test]
    fn witness_segment_counts_events_and_flags_silence() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let dir = root.join("sess-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(EVENTS_FILE), "{}\n{}\n{}\n").unwrap();

        let seg = witness_segment(r#"{"session_id":"sess-1"}"#, root);
        assert_eq!(seg, format!("{SEG_RECORDING} 3ev"));

        // Unknown session, malformed stdin, and traversal ids all read as
        // not-recording rather than erroring or escaping the store.
        for stdin in [
            r#"{"session_id":"nope"}"#,
            "not json",
            r#"{"session_id":"../escape"}"#,
            r#"{"session_id":""}"#,
        ] {
            assert_eq!(witness_segment(stdin, root), SEG_NOT_RECORDING);
        }
    }

    #[tokio::test]
    async fn render_composes_wrapped_output_before_ours() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let dir = root.join("s");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(EVENTS_FILE), "{}\n").unwrap();

        let wrapped = wrapped_command("printf 'THEIRS\\nextra'").unwrap();
        let payload = wrapped
            .strip_prefix(STATUSLINE_COMMAND)
            .unwrap()
            .trim_start()
            .strip_prefix(WRAP_FLAG)
            .unwrap()
            .trim()
            .to_string();

        let line = render(r#"{"session_id":"s"}"#, root, Some(&payload)).await;
        assert_eq!(line, format!("THEIRS{SEG_SEPARATOR}{SEG_RECORDING} 1ev"));
    }

    #[tokio::test]
    async fn render_degrades_to_ours_when_original_fails_or_is_garbage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();

        // Failing command → our segment only.
        let wrapped = wrapped_command("exit 3").unwrap();
        let payload = wrapped.split_whitespace().last().unwrap().to_string();
        let line = render(r#"{"session_id":"s"}"#, root, Some(&payload)).await;
        assert_eq!(line, SEG_NOT_RECORDING);

        // Undecodable payload → our segment only.
        let line = render(r#"{"session_id":"s"}"#, root, Some("garbage")).await;
        assert_eq!(line, SEG_NOT_RECORDING);
    }

    #[tokio::test]
    async fn render_never_nests_itself() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wrapped = wrapped_command("agent-witness statusline --wrap-v1 xyz").unwrap();
        let payload = wrapped.split_whitespace().last().unwrap().to_string();
        let line = render(r#"{"session_id":"s"}"#, tmp.path(), Some(&payload)).await;
        assert_eq!(line, SEG_NOT_RECORDING);
    }
}
