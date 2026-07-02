//! `agent-witness init`: safe, idempotent registration of the Claude Code hooks
//! that drive `agent-witness emit`.
//!
//! Editing a user's `settings.json` is the one place we mutate state outside our
//! own store, so the contract is strict (see CLAUDE.md "Key Gotchas"):
//!
//! - **Merge, never clobber.** Existing hooks — ours or the user's — are kept.
//!   We parse the file as an untyped [`serde_json::Value`] so unknown fields are
//!   preserved verbatim rather than dropped by a typed struct.
//! - **Idempotent.** Our own entries are detected by the command containing
//!   [`OWN_MARKER`]; a second `init` adds nothing.
//! - **Backed up.** An existing file is copied to `settings.json.bak-<ts>`
//!   before it is rewritten. The timestamp comes from an injected [`Clock`].
//! - **Non-destructive on bad input.** A file that is not valid JSON (or not a
//!   JSON object) aborts with a message telling the user to fix it; we never
//!   attempt to repair or overwrite it.
//! - **`--remove` is surgical.** It strips only our entries (and the groups /
//!   event arrays they leave empty), never the user's.
//!
//! The settings path is passed in explicitly so tests exercise everything
//! against a tempdir and never touch the real home directory.

use std::path::{Path, PathBuf};

use agent_witness_core::Clock;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::paths;

/// The hook command Claude Code runs; forwards one hook payload to `emit`.
const EMIT_COMMAND: &str = "agent-witness emit";
/// Substring identifying hook entries this tool owns. Any hook command
/// containing it is treated as ours for idempotent install and clean `--remove`.
const OWN_MARKER: &str = "agent-witness emit";

/// Top-level key holding all hook configuration.
const HOOKS_KEY: &str = "hooks";
/// Key on a matcher group holding the tool-name pattern.
const MATCHER_KEY: &str = "matcher";
/// Key on a matcher group holding the list of command hooks.
const GROUP_HOOKS_KEY: &str = "hooks";
/// Key on a hook entry holding the shell command.
const COMMAND_KEY: &str = "command";
/// Key on a hook entry holding its type discriminator.
const TYPE_KEY: &str = "type";
/// Hook type value for a shell command.
const COMMAND_TYPE: &str = "command";
/// Matcher pattern that matches every tool (PreToolUse / PostToolUse).
const MATCH_ALL: &str = "*";

/// Directory under home / project root holding Claude Code settings.
const CLAUDE_DIR: &str = ".claude";
/// Settings file name.
const SETTINGS_FILE: &str = "settings.json";
/// Prefix for the timestamped backup file (`settings.json.bak-<ts>`).
const BACKUP_PREFIX: &str = ".bak-";

/// Hook events we register, paired with whether the event takes a tool matcher.
/// PreToolUse / PostToolUse are per-tool (matcher `*`); Stop fires once per turn
/// and takes no matcher (confirmed against the Claude Code hooks reference).
const MANAGED_HOOKS: &[(&str, bool)] =
    &[("PreToolUse", true), ("PostToolUse", true), ("Stop", false)];

/// What `init` did — surfaced for the CLI report and asserted in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitOutcome {
    /// One or more hook entries were added.
    Installed,
    /// All our entries were already present; nothing changed.
    AlreadyInstalled,
    /// One or more of our entries were removed.
    Removed,
    /// No entries of ours were present to remove; nothing changed.
    NothingToRemove,
}

/// Result of an `init` run: what changed and the backup file (if one was made).
#[derive(Debug, Clone)]
pub struct InitReport {
    /// Outcome of the run.
    pub outcome: InitOutcome,
    /// Path of the backup written before rewriting an existing file, if any.
    pub backup: Option<PathBuf>,
}

/// Resolve the settings file to edit: `~/.claude/settings.json`, or
/// `./.claude/settings.json` when `project` is set.
pub fn resolve_settings_path(project: bool) -> Result<PathBuf> {
    let base = if project {
        std::env::current_dir().context("cannot determine current directory")?
    } else {
        paths::home_dir()?
    };
    Ok(base.join(CLAUDE_DIR).join(SETTINGS_FILE))
}

/// Install (or, with `remove`, uninstall) our hook entries in `settings_path`.
///
/// Backs up an existing file before rewriting it and only writes when something
/// actually changes. Aborts without modifying the file if it is not valid JSON
/// or not a JSON object.
pub fn run_init(settings_path: &Path, remove: bool, clock: &dyn Clock) -> Result<InitReport> {
    let loaded = load_settings(settings_path)?;

    if remove {
        let Some(mut root) = loaded else {
            return Ok(InitReport {
                outcome: InitOutcome::NothingToRemove,
                backup: None,
            });
        };
        if !remove_entries(&mut root)? {
            return Ok(InitReport {
                outcome: InitOutcome::NothingToRemove,
                backup: None,
            });
        }
        let backup = backup_existing(settings_path, clock)?;
        write_settings(settings_path, &root)?;
        return Ok(InitReport {
            outcome: InitOutcome::Removed,
            backup,
        });
    }

    let mut root = loaded.unwrap_or_default();
    if !install_entries(&mut root)? {
        return Ok(InitReport {
            outcome: InitOutcome::AlreadyInstalled,
            backup: None,
        });
    }
    let backup = backup_existing(settings_path, clock)?;
    write_settings(settings_path, &root)?;
    Ok(InitReport {
        outcome: InitOutcome::Installed,
        backup,
    })
}

/// Load and parse `settings.json` into its top-level object.
///
/// Returns `Ok(None)` if the file does not exist, `Ok(Some(empty))` for an
/// empty file, and an error (without touching the file) if it is not valid JSON
/// or not a JSON object.
fn load_settings(path: &Path) -> Result<Option<Map<String, Value>>> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(anyhow!("cannot read settings file {}: {e}", path.display())),
    };
    if raw.trim().is_empty() {
        return Ok(Some(Map::new()));
    }
    let value: Value = serde_json::from_str(&raw).map_err(|e| {
        anyhow!(
            "settings file {} is not valid JSON ({e}); refusing to modify it. \
             Fix the JSON (or delete the file) and re-run.",
            path.display()
        )
    })?;
    match value {
        Value::Object(map) => Ok(Some(map)),
        _ => bail!(
            "settings file {} is not a JSON object; refusing to modify it.",
            path.display()
        ),
    }
}

/// Ensure each managed hook event carries our entry. Returns `true` if anything
/// was added. Aborts if an existing `hooks` value or event array has the wrong
/// JSON shape (so we never clobber a user's unexpected structure).
fn install_entries(root: &mut Map<String, Value>) -> Result<bool> {
    let hooks = hooks_object_mut(root)?;
    let mut changed = false;
    for (event, use_matcher) in MANAGED_HOOKS {
        let array = event_array_mut(hooks, event)?;
        if array.iter().any(group_contains_own_command) {
            continue; // already registered — idempotent
        }
        array.push(new_group(*use_matcher));
        changed = true;
    }
    Ok(changed)
}

/// Remove our hook entries and any groups / event arrays they leave empty,
/// touching only structures that actually contained our command. Returns `true`
/// if anything was removed.
fn remove_entries(root: &mut Map<String, Value>) -> Result<bool> {
    let Some(hooks) = root.get_mut(HOOKS_KEY).and_then(Value::as_object_mut) else {
        return Ok(false); // no hooks object => nothing of ours can be here
    };
    let mut changed = false;
    let event_names: Vec<String> = hooks.keys().cloned().collect();
    for event in event_names {
        let Some(array) = hooks.get_mut(&event).and_then(Value::as_array_mut) else {
            continue;
        };
        let mut touched = false;
        array.retain_mut(|group| {
            if !strip_own_from_group(group) {
                return true; // untouched user group — keep verbatim
            }
            touched = true;
            !group_hooks_empty(group) // drop only groups our removal emptied
        });
        if touched {
            changed = true;
            if array.is_empty() {
                hooks.remove(&event);
            }
        }
    }
    if changed && hooks.is_empty() {
        root.remove(HOOKS_KEY);
    }
    Ok(changed)
}

/// Get the `hooks` object, creating it if absent. Errors if it exists but is not
/// an object.
fn hooks_object_mut(root: &mut Map<String, Value>) -> Result<&mut Map<String, Value>> {
    root.entry(HOOKS_KEY)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("settings `{HOOKS_KEY}` is present but is not a JSON object; refusing to modify it."))
}

/// Get the array for one hook event, creating it if absent. Errors if it exists
/// but is not an array.
fn event_array_mut<'a>(
    hooks: &'a mut Map<String, Value>,
    event: &str,
) -> Result<&'a mut Vec<Value>> {
    hooks
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow!("settings `{HOOKS_KEY}.{event}` is present but is not a JSON array; refusing to modify it."))
}

/// Build a fresh matcher group registering our emit command.
fn new_group(use_matcher: bool) -> Value {
    let hook = json!({ TYPE_KEY: COMMAND_TYPE, COMMAND_KEY: EMIT_COMMAND });
    if use_matcher {
        json!({ MATCHER_KEY: MATCH_ALL, GROUP_HOOKS_KEY: [hook] })
    } else {
        json!({ GROUP_HOOKS_KEY: [hook] })
    }
}

/// Whether a matcher group contains a hook command that is ours.
fn group_contains_own_command(group: &Value) -> bool {
    group
        .get(GROUP_HOOKS_KEY)
        .and_then(Value::as_array)
        .is_some_and(|hooks| hooks.iter().any(hook_is_own))
}

/// Whether a hook entry's command is one we installed.
fn hook_is_own(hook: &Value) -> bool {
    hook.get(COMMAND_KEY)
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(OWN_MARKER))
}

/// Strip our hook entries from a group's `hooks` array. Returns `true` if any
/// were removed. Non-object groups and groups without a hooks array are left
/// untouched.
fn strip_own_from_group(group: &mut Value) -> bool {
    let Some(hooks) = group.get_mut(GROUP_HOOKS_KEY).and_then(Value::as_array_mut) else {
        return false;
    };
    let before = hooks.len();
    hooks.retain(|hook| !hook_is_own(hook));
    hooks.len() != before
}

/// Whether a group's `hooks` array is now empty (used to drop groups we emptied).
fn group_hooks_empty(group: &Value) -> bool {
    group
        .get(GROUP_HOOKS_KEY)
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
}

/// Copy an existing settings file to a timestamped backup before it is rewritten.
/// Returns `None` when there is no existing file to preserve.
fn backup_existing(path: &Path, clock: &dyn Clock) -> Result<Option<PathBuf>> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(anyhow!("cannot read {} for backup: {e}", path.display())),
    };
    let backup = backup_path(path, clock.now_ms());
    std::fs::write(&backup, &raw)
        .with_context(|| format!("cannot write backup {}", backup.display()))?;
    Ok(Some(backup))
}

/// `settings.json` -> `settings.json.bak-<ts>` in the same directory.
fn backup_path(path: &Path, ts: i64) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(format!("{BACKUP_PREFIX}{ts}"));
    path.with_file_name(name)
}

/// Serialize and write the settings object, creating parent directories as
/// needed. Written pretty-printed with a trailing newline.
fn write_settings(path: &Path, root: &Map<String, Value>) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut text = serde_json::to_string_pretty(&Value::Object(root.clone()))
        .context("cannot serialize settings")?;
    text.push('\n');
    std::fs::write(path, text).with_context(|| format!("cannot write {}", path.display()))
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
        let raw = std::fs::read_to_string(path).expect("read settings");
        serde_json::from_str(&raw).expect("valid json")
    }

    /// Number of groups registering our command across a whole event array.
    fn own_group_count(root: &Value, event: &str) -> usize {
        root["hooks"][event]
            .as_array()
            .map(|groups| {
                groups
                    .iter()
                    .filter(|g| group_contains_own_command(g))
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn install_into_missing_file_creates_all_events_without_backup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(".claude").join("settings.json");

        let report = run_init(&path, false, &clock()).unwrap();
        assert_eq!(report.outcome, InitOutcome::Installed);
        assert!(report.backup.is_none(), "fresh file has nothing to back up");

        let root = read_json(&path);
        // PreToolUse / PostToolUse carry matcher "*"; Stop carries none.
        assert_eq!(root["hooks"]["PreToolUse"][0]["matcher"], json!(MATCH_ALL));
        assert_eq!(root["hooks"]["PostToolUse"][0]["matcher"], json!(MATCH_ALL));
        assert!(root["hooks"]["Stop"][0].get("matcher").is_none());
        for (event, _) in MANAGED_HOOKS {
            let command = &root["hooks"][*event][0]["hooks"][0]["command"];
            assert_eq!(command, &json!(EMIT_COMMAND), "for {event}");
        }
    }

    #[test]
    fn install_into_existing_file_creates_backup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, "{}\n").unwrap();

        let report = run_init(&path, false, &clock()).unwrap();
        assert_eq!(report.outcome, InitOutcome::Installed);

        let backup = report.backup.expect("existing file must be backed up");
        assert_eq!(backup, backup_path(&path, TS));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "{}\n");
    }

    #[test]
    fn install_merges_and_preserves_user_hooks_and_unknown_fields() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
              "model": "opus",
              "hooks": {
                "PreToolUse": [
                  { "matcher": "Bash", "hooks": [ { "type": "command", "command": "user-audit" } ] }
                ]
              }
            }"#,
        )
        .unwrap();

        let report = run_init(&path, false, &clock()).unwrap();
        assert_eq!(report.outcome, InitOutcome::Installed);

        let root = read_json(&path);
        // Unknown top-level field preserved verbatim.
        assert_eq!(root["model"], json!("opus"));
        // User's PreToolUse group is intact...
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(pre
            .iter()
            .any(|g| g["hooks"][0]["command"] == json!("user-audit")));
        // ...and ours was appended alongside it.
        assert_eq!(own_group_count(&root, "PreToolUse"), 1);
        assert_eq!(own_group_count(&root, "PostToolUse"), 1);
        assert_eq!(own_group_count(&root, "Stop"), 1);
    }

    #[test]
    fn install_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        assert_eq!(
            run_init(&path, false, &clock()).unwrap().outcome,
            InitOutcome::Installed
        );
        let first = read_json(&path);

        let second = run_init(&path, false, &clock()).unwrap();
        assert_eq!(second.outcome, InitOutcome::AlreadyInstalled);
        assert!(second.backup.is_none(), "no-op must not create a backup");

        // No duplicate entries were added.
        assert_eq!(read_json(&path), first);
        for (event, _) in MANAGED_HOOKS {
            assert_eq!(own_group_count(&first, event), 1, "for {event}");
        }
    }

    #[test]
    fn remove_strips_only_our_entries() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
              "hooks": {
                "PreToolUse": [
                  { "matcher": "Bash", "hooks": [ { "type": "command", "command": "user-audit" } ] }
                ]
              }
            }"#,
        )
        .unwrap();

        run_init(&path, false, &clock()).unwrap();
        let report = run_init(&path, true, &clock()).unwrap();
        assert_eq!(report.outcome, InitOutcome::Removed);
        assert!(report.backup.is_some());

        let root = read_json(&path);
        // User group survives; ours is gone from PreToolUse.
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["hooks"][0]["command"], json!("user-audit"));
        assert_eq!(own_group_count(&root, "PreToolUse"), 0);
        // Events that only ever held our entry are cleaned up entirely.
        assert!(root["hooks"].get("PostToolUse").is_none());
        assert!(root["hooks"].get("Stop").is_none());
    }

    #[test]
    fn remove_of_only_our_entries_clears_hooks_object() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        run_init(&path, false, &clock()).unwrap();
        assert_eq!(
            run_init(&path, true, &clock()).unwrap().outcome,
            InitOutcome::Removed
        );

        let root = read_json(&path);
        // Nothing of ours remains and the empty hooks container is dropped.
        assert!(
            root.get("hooks").is_none(),
            "empty hooks object should be removed"
        );
    }

    #[test]
    fn remove_with_no_entries_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, r#"{"model":"opus"}"#).unwrap();

        let report = run_init(&path, true, &clock()).unwrap();
        assert_eq!(report.outcome, InitOutcome::NothingToRemove);
        assert!(report.backup.is_none());
        // Untouched.
        assert_eq!(read_json(&path), json!({ "model": "opus" }));
    }

    #[test]
    fn remove_on_missing_file_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        let report = run_init(&path, true, &clock()).unwrap();
        assert_eq!(report.outcome, InitOutcome::NothingToRemove);
        assert!(!path.exists(), "must not create a file on remove");
    }

    #[test]
    fn broken_json_aborts_without_modifying_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        let original = "{ this is not json";
        std::fs::write(&path, original).unwrap();

        let err = run_init(&path, false, &clock()).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "message: {err}");
        // File is unchanged and no backup was made.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!backup_path(&path, TS).exists());
    }

    #[test]
    fn non_object_json_aborts_without_modifying_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, "[1, 2, 3]").unwrap();

        let err = run_init(&path, false, &clock()).unwrap_err();
        assert!(
            err.to_string().contains("not a JSON object"),
            "message: {err}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[1, 2, 3]");
    }

    #[test]
    fn resolve_settings_path_project_ends_in_claude_settings() {
        let path = resolve_settings_path(true).unwrap();
        assert!(path.ends_with(Path::new(CLAUDE_DIR).join(SETTINGS_FILE)));
    }
}
