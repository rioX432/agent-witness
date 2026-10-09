//! `agent-witness init`: safe, idempotent registration of agent hooks
//! that drive `agent-witness emit`.
//!
//! Editing a user's `settings.json` is the one place we mutate state outside our
//! own store, so the contract is strict (see CLAUDE.md "Key Gotchas"):
//!
//! - **Merge, never clobber.** Existing hooks — ours or the user's — are kept.
//!   We parse the file as an untyped [`serde_json::Value`] so unknown fields are
//!   preserved verbatim rather than dropped by a typed struct.
//! - **Idempotent.** Only command handlers invoking `agent-witness emit` belong
//!   to us; references to that command in foreign handlers remain untouched.
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

use agent_witness_core::{AgentName, Clock};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::{backup, paths};

const OWN_MARKER: &str = "agent-witness emit";
const AGENT_FLAG: &str = "--agent";
const EMIT_SUBCOMMAND: &str = "emit";
const EXECUTABLE_NAME: &str = "agent-witness";

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
const CODEX_DIR: &str = ".codex";
const CODEX_HOOKS_FILE: &str = "hooks.json";

/// Claude hook events, paired with whether the event uses a matcher.
///
/// Tool events use matcher `*`; Notification omits it to capture every type.
/// Lifecycle events also omit it. The official contract is mirrored by
/// `tools/fixtures/capture.sh`. This set MUST stay identical to the events
/// capture.sh registers, or fixtures and real sessions drift and the pipeline
/// silently loses events (issue #26); `init_and_capture_register_same_events`
/// pins that parity.
const MANAGED_HOOKS: &[(&str, bool)] = &[
    ("SessionStart", false),
    ("UserPromptSubmit", false),
    ("PreToolUse", true),
    ("PostToolUse", true),
    ("PostToolUseFailure", true),
    ("Notification", false),
    ("PermissionRequest", true),
    ("PermissionDenied", true),
    ("Stop", false),
    ("SessionEnd", false),
];

const CODEX_HOOKS: &[(&str, bool)] = &[
    ("SessionStart", false),
    ("UserPromptSubmit", false),
    ("PreToolUse", true),
    ("PostToolUse", true),
    ("Stop", false),
    ("SessionEnd", false),
    ("PermissionRequest", true),
    ("SubagentStart", true),
    ("SubagentStop", true),
    ("Interrupt", false),
];

/// What `init` did — surfaced for the CLI report and asserted in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitOutcome {
    /// One or more hook entries were added or upgraded.
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
    resolve_agent_settings_path(project, AgentName::ClaudeCode)
}

pub fn resolve_codex_settings_path(project: bool) -> Result<PathBuf> {
    resolve_agent_settings_path(project, AgentName::Codex)
}

fn resolve_agent_settings_path(project: bool, agent: AgentName) -> Result<PathBuf> {
    let base = if project {
        std::env::current_dir().context("cannot determine current directory")?
    } else {
        paths::home_dir()?
    };
    Ok(settings_path_in(&base, agent))
}

fn settings_path_in(base: &Path, agent: AgentName) -> PathBuf {
    match agent {
        AgentName::ClaudeCode => base.join(CLAUDE_DIR).join(SETTINGS_FILE),
        AgentName::Codex => base.join(CODEX_DIR).join(CODEX_HOOKS_FILE),
    }
}

/// Install (or, with `remove`, uninstall) our hook entries in `settings_path`.
///
/// Backs up an existing file before rewriting it and only writes when something
/// actually changes. Aborts without modifying the file if it is not valid JSON
/// or not a JSON object.
pub fn run_init(settings_path: &Path, remove: bool, clock: &dyn Clock) -> Result<InitReport> {
    run_init_for_agent(settings_path, remove, clock, AgentName::ClaudeCode)
}

pub fn run_init_for_agent(
    settings_path: &Path,
    remove: bool,
    clock: &dyn Clock,
    agent: AgentName,
) -> Result<InitReport> {
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
    if !install_entries(&mut root, agent)? {
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
pub(crate) fn load_settings(path: &Path) -> Result<Option<Map<String, Value>>> {
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
/// changed. Aborts if an existing `hooks` value or event array has the wrong
/// JSON shape (so we never clobber a user's unexpected structure).
fn install_entries(root: &mut Map<String, Value>, agent: AgentName) -> Result<bool> {
    let hooks = hooks_object_mut(root)?;
    let mut changed = false;
    if agent == AgentName::Codex {
        for event in ["PostToolUseFailure", "Notification", "PermissionDenied"] {
            let Some(array) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
                continue;
            };
            let mut touched = false;
            array.retain_mut(|group| {
                if !strip_own_from_group(group) {
                    return true;
                }
                touched = true;
                !group_hooks_empty(group)
            });
            if touched {
                changed = true;
                if array.is_empty() {
                    hooks.remove(event);
                }
            }
        }
    }
    let managed = match agent {
        AgentName::ClaudeCode => MANAGED_HOOKS,
        AgentName::Codex => CODEX_HOOKS,
    };
    for (event, use_matcher) in managed {
        let array = event_array_mut(hooks, event)?;
        let mut found = false;
        for group in array.iter_mut() {
            let Some(entries) = group.get_mut(GROUP_HOOKS_KEY).and_then(Value::as_array_mut) else {
                continue;
            };
            for entry in entries.iter_mut().filter(|entry| hook_is_own(entry)) {
                found = true;
                if let Some(command) = entry.get(COMMAND_KEY).and_then(Value::as_str) {
                    let upgraded = command_with_agent(command, agent);
                    if upgraded != command {
                        entry[COMMAND_KEY] = Value::String(upgraded);
                        changed = true;
                    }
                }
            }
        }
        if !found {
            array.push(new_group(*use_matcher, agent));
            changed = true;
        }
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
fn new_group(use_matcher: bool, agent: AgentName) -> Value {
    let command = format!("{OWN_MARKER} {AGENT_FLAG} {}", agent.label());
    let hook = json!({ TYPE_KEY: COMMAND_TYPE, COMMAND_KEY: command });
    if use_matcher {
        json!({ MATCHER_KEY: MATCH_ALL, GROUP_HOOKS_KEY: [hook] })
    } else {
        json!({ GROUP_HOOKS_KEY: [hook] })
    }
}

/// Whether a matcher group contains a hook command that is ours.
#[cfg(test)]
fn group_contains_own_command(group: &Value) -> bool {
    group
        .get(GROUP_HOOKS_KEY)
        .and_then(Value::as_array)
        .is_some_and(|hooks| hooks.iter().any(hook_is_own))
}

/// Whether a hook entry's command is one we installed.
fn hook_is_own(hook: &Value) -> bool {
    if hook.get(TYPE_KEY).and_then(Value::as_str) != Some(COMMAND_TYPE) {
        return false;
    }
    let Some(command) = hook.get(COMMAND_KEY).and_then(Value::as_str) else {
        return false;
    };
    let mut parts = command.split_whitespace();
    let Some(executable) = parts.next() else {
        return false;
    };
    Path::new(executable.trim_matches(['\'', '"']))
        .file_name()
        .is_some_and(|name| name == EXECUTABLE_NAME)
        && parts.next() == Some(EMIT_SUBCOMMAND)
}

fn command_with_agent(command: &str, agent: AgentName) -> String {
    let label = agent.label();
    let mut parts = command.split_whitespace().scan(0, |cursor, part| {
        let start = command[*cursor..].find(part)? + *cursor;
        *cursor = start + part.len();
        Some((start, *cursor, part))
    });
    parts.next();
    let Some((_, emit_end, _)) = parts.next() else {
        return command.to_string();
    };
    while let Some((start, end, part)) = parts.next() {
        if part == AGENT_FLAG {
            if let Some((value_start, value_end, _)) = parts.next() {
                return format!(
                    "{}{label}{}",
                    &command[..value_start],
                    &command[value_end..]
                );
            }
        }
        if part.starts_with(&format!("{AGENT_FLAG}=")) {
            return format!(
                "{}{AGENT_FLAG}={label}{}",
                &command[..start],
                &command[end..]
            );
        }
    }
    format!(
        "{} {AGENT_FLAG} {label}{}",
        &command[..emit_end],
        &command[emit_end..]
    )
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
    Ok(Some(backup::write_backup(path, &raw, clock.now_ms())?))
}

/// Serialize and write the settings object, creating parent directories as
/// needed. Written pretty-printed with a trailing newline.
pub(crate) fn write_settings(path: &Path, root: &Map<String, Value>) -> Result<()> {
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
        // PreToolUse / PostToolUse carry matcher "*"; the non-tool events don't.
        assert_eq!(root["hooks"]["PreToolUse"][0]["matcher"], json!(MATCH_ALL));
        assert_eq!(root["hooks"]["PostToolUse"][0]["matcher"], json!(MATCH_ALL));
        for event in ["SessionStart", "UserPromptSubmit", "Stop"] {
            assert!(
                root["hooks"][event][0].get("matcher").is_none(),
                "{event} must carry no matcher"
            );
        }
        for (event, _) in MANAGED_HOOKS {
            let command = &root["hooks"][*event][0]["hooks"][0]["command"];
            assert_eq!(
                command,
                &json!("agent-witness emit --agent claude-code"),
                "for {event}"
            );
        }
    }

    #[test]
    fn install_upgrades_partial_legacy_set_without_duplicating() {
        // Migration path: an install from before issue #26 registered only the
        // three tool/turn hooks. Re-running init must ADD the two missing events
        // (SessionStart / UserPromptSubmit) and leave the existing three intact,
        // with no duplicate entries.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{
              "hooks": {
                "PreToolUse":  [ { "matcher": "*", "hooks": [ { "type": "command", "command": "agent-witness emit" } ] } ],
                "PostToolUse": [ { "matcher": "*", "hooks": [ { "type": "command", "command": "agent-witness emit" } ] } ],
                "Stop":        [ { "hooks": [ { "type": "command", "command": "agent-witness emit" } ] } ]
              }
            }"#,
        )
        .unwrap();

        let report = run_init(&path, false, &clock()).unwrap();
        assert_eq!(report.outcome, InitOutcome::Installed);

        let root = read_json(&path);
        // Every managed event registered exactly once.
        for (event, _) in MANAGED_HOOKS {
            assert_eq!(own_group_count(&root, event), 1, "for {event}");
        }
        // The two previously missing events are now present with no matcher.
        for event in ["SessionStart", "UserPromptSubmit"] {
            assert!(root["hooks"][event][0].get("matcher").is_none());
        }

        // And a further re-run is a clean no-op (full set is idempotent).
        let again = run_init(&path, false, &clock()).unwrap();
        assert_eq!(again.outcome, InitOutcome::AlreadyInstalled);
    }

    #[test]
    fn reinstall_adds_failure_hook_and_preserves_existing_install() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        run_init(&path, false, &clock()).unwrap();
        let mut old = read_json(&path);
        old["hooks"]
            .as_object_mut()
            .unwrap()
            .remove("PostToolUseFailure");
        old["hooks"]["PostToolUseFailure"] = json!([
            {"matcher": "Bash", "hooks": [{"type": "command", "command": "custom-failure-hook"}]}
        ]);
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();

        let report = run_init(&path, false, &clock()).unwrap();
        assert_eq!(report.outcome, InitOutcome::Installed);
        assert_eq!(read_json(report.backup.as_ref().unwrap()), old);
        let root = read_json(&path);
        for (event, _) in MANAGED_HOOKS {
            assert_eq!(own_group_count(&root, event), 1, "for {event}");
            if *event != "PostToolUseFailure" {
                assert_eq!(root["hooks"][*event], old["hooks"][*event]);
            }
        }
        assert_eq!(
            root["hooks"]["PostToolUseFailure"][0],
            old["hooks"]["PostToolUseFailure"][0]
        );
        assert_eq!(
            root["hooks"]["PostToolUseFailure"][1]["matcher"],
            json!(MATCH_ALL)
        );
        assert_eq!(
            run_init(&path, false, &clock()).unwrap().outcome,
            InitOutcome::AlreadyInstalled
        );
    }

    #[test]
    fn init_and_capture_register_same_events() {
        // Regression guard for issue #26: fixtures come from capture.sh, so if
        // its hook set and init's diverge, fixtures exercise events real users
        // never record (or vice versa) and tests miss the gap. Assert the two
        // register the identical (event, uses-matcher) set.
        let script = std::fs::read_to_string(capture_sh_path()).expect("read capture.sh");
        let mut from_capture = capture_sh_event_shapes(&script);
        let mut from_init: Vec<(String, bool)> = MANAGED_HOOKS
            .iter()
            .map(|(name, m)| ((*name).to_string(), *m))
            .collect();
        from_capture.sort();
        from_init.sort();
        assert_eq!(
            from_init, from_capture,
            "init and capture.sh must register the same hook events with the same matcher shape"
        );
    }

    #[test]
    fn codex_registers_only_documented_hooks_and_shares_permission_capture_shape() {
        let script = std::fs::read_to_string(capture_sh_path()).unwrap();
        let capture = capture_sh_event_shapes(&script);
        assert!(capture.contains(&("PermissionRequest".into(), true)));
        assert!(CODEX_HOOKS.contains(&("PermissionRequest", true)));
        assert!(CODEX_HOOKS.contains(&("Interrupt", false)));
        for unsupported in ["Notification", "PermissionDenied", "PostToolUseFailure"] {
            assert!(!CODEX_HOOKS.iter().any(|(name, _)| *name == unsupported));
        }
        assert!(!MANAGED_HOOKS.iter().any(|(name, _)| *name == "Interrupt"));
    }

    /// Absolute path to `tools/fixtures/capture.sh` from this crate's manifest.
    fn capture_sh_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tools")
            .join("fixtures")
            .join("capture.sh")
    }

    /// Mechanically extract capture.sh's registered `(event, uses_matcher)` set
    /// from its jq hook spec. Each event line reads `EventName: with_matcher` or
    /// `EventName: no_matcher`; the `def with_matcher:` / `def no_matcher:`
    /// definition lines are skipped because their name contains a space.
    fn capture_sh_event_shapes(script: &str) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        for line in script.lines() {
            let Some((name_part, rest)) = line.trim().split_once(':') else {
                continue;
            };
            let name = name_part.trim();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
                continue;
            }
            let use_matcher = if rest.contains("with_matcher") {
                true
            } else if rest.contains("no_matcher") {
                false
            } else {
                continue;
            };
            out.push((name.to_string(), use_matcher));
        }
        out
    }

    #[test]
    fn install_into_existing_file_creates_backup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, "{}\n").unwrap();

        let report = run_init(&path, false, &clock()).unwrap();
        assert_eq!(report.outcome, InitOutcome::Installed);

        let backup = report.backup.expect("existing file must be backed up");
        assert_eq!(backup, crate::backup::backup_path(&path, TS));
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
        assert!(!crate::backup::backup_path(&path, TS).exists());
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

    #[test]
    fn codex_install_into_injected_home_registers_identity_and_lifecycle_events() {
        let home = tempfile::TempDir::new().unwrap();
        let path = settings_path_in(home.path(), AgentName::Codex);

        let report = run_init_for_agent(&path, false, &clock(), AgentName::Codex).unwrap();

        assert_eq!(report.outcome, InitOutcome::Installed);
        assert!(report.backup.is_none());
        assert_eq!(path, home.path().join(".codex/hooks.json"));
        assert!(!home.path().join(CLAUDE_DIR).exists());
        let root = read_json(&path);
        assert_eq!(
            root[HOOKS_KEY].as_object().unwrap().len(),
            CODEX_HOOKS.len()
        );
        for (event, use_matcher) in CODEX_HOOKS {
            let group = &root[HOOKS_KEY][*event][0];
            assert_eq!(
                group[GROUP_HOOKS_KEY][0][COMMAND_KEY],
                json!("agent-witness emit --agent codex"),
                "for {event}"
            );
            assert_eq!(group.get(MATCHER_KEY).is_some(), *use_matcher);
        }
    }

    #[test]
    fn codex_upgrade_removes_only_owned_unsupported_hooks() {
        let home = tempfile::TempDir::new().unwrap();
        let path = settings_path_in(home.path(), AgentName::Codex);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = json!({"hooks": {"PostToolUseFailure": [{"matcher":"*", "hooks":[
            {"type":"command", "command":"agent-witness emit --agent codex"},
            {"type":"command", "command":"foreign-handler"}
        ]}]}});
        std::fs::write(&path, serde_json::to_string(&original).unwrap()).unwrap();
        let report = run_init_for_agent(&path, false, &clock(), AgentName::Codex).unwrap();
        assert_eq!(report.outcome, InitOutcome::Installed);
        assert!(report.backup.is_some());
        let root = read_json(&path);
        assert_eq!(own_group_count(&root, "PostToolUseFailure"), 0);
        assert_eq!(
            root["hooks"]["PostToolUseFailure"][0]["hooks"],
            json!([{ "type":"command", "command":"foreign-handler" }])
        );
        assert_eq!(
            run_init_for_agent(&path, false, &clock(), AgentName::Codex)
                .unwrap()
                .outcome,
            InitOutcome::AlreadyInstalled
        );
    }

    #[test]
    fn codex_install_upgrades_handwritten_entry_in_place_and_preserves_foreign_fields() {
        let home = tempfile::TempDir::new().unwrap();
        let path = settings_path_in(home.path(), AgentName::Codex);
        let foreign_hook = json!({
            "type": "command", "command": "echo 'agent-witness emit'", "timeout": 10
        });
        let foreign_group = json!({
            "matcher": "Edit", "hooks": [{"type": "command", "command": "audit-edits"}]
        });
        let original = json!({
            "description": "custom hooks",
            "hooks": {
                "PostToolUse": [{
                    "matcher": "Bash", "groupExtra": true,
                    "hooks": [
                        {"type": "command", "command": "agent-witness emit --socket /tmp/witness-test.sock", "timeout": 17, "statusMessage": "Recording"},
                        foreign_hook.clone()
                    ]
                }, foreign_group.clone()],
                "CustomEvent": [{"hooks": [foreign_hook.clone()]}]
            }
        });
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original_bytes = serde_json::to_vec_pretty(&original).unwrap();
        std::fs::write(&path, &original_bytes).unwrap();

        let report = run_init_for_agent(&path, false, &clock(), AgentName::Codex).unwrap();

        assert_eq!(report.outcome, InitOutcome::Installed);
        let backup = report.backup.unwrap();
        assert_eq!(backup, crate::backup::backup_path(&path, TS));
        assert_eq!(std::fs::read(backup).unwrap(), original_bytes);
        let root = read_json(&path);
        let groups = root[HOOKS_KEY]["PostToolUse"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0][MATCHER_KEY], "Bash");
        assert_eq!(groups[0]["groupExtra"], true);
        assert_eq!(
            groups[0][GROUP_HOOKS_KEY][0][COMMAND_KEY],
            "agent-witness emit --agent codex --socket /tmp/witness-test.sock"
        );
        assert_eq!(groups[0][GROUP_HOOKS_KEY][0]["timeout"], 17);
        assert_eq!(groups[0][GROUP_HOOKS_KEY][0]["statusMessage"], "Recording");
        assert_eq!(groups[0][GROUP_HOOKS_KEY][1], foreign_hook);
        assert_eq!(groups[1], foreign_group);
        assert_eq!(root["description"], original["description"]);
        assert_eq!(
            root[HOOKS_KEY]["CustomEvent"],
            original[HOOKS_KEY]["CustomEvent"]
        );
        for (event, _) in CODEX_HOOKS {
            assert_eq!(own_group_count(&root, event), 1, "for {event}");
        }
    }

    #[test]
    fn codex_second_install_changes_neither_file_nor_backup() {
        let home = tempfile::TempDir::new().unwrap();
        let path = settings_path_in(home.path(), AgentName::Codex);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}\n").unwrap();
        let first = run_init_for_agent(&path, false, &clock(), AgentName::Codex).unwrap();
        let first_bytes = std::fs::read(&path).unwrap();
        let backup_path = first.backup.unwrap();
        let backup_bytes = std::fs::read(&backup_path).unwrap();

        let second =
            run_init_for_agent(&path, false, &FixedClock(TS + 1), AgentName::Codex).unwrap();

        assert_eq!(second.outcome, InitOutcome::AlreadyInstalled);
        assert!(second.backup.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), first_bytes);
        assert_eq!(std::fs::read(&backup_path).unwrap(), backup_bytes);
        assert!(!crate::backup::backup_path(&path, TS + 1).exists());
    }

    #[test]
    fn codex_remove_preserves_foreign_handlers_even_when_they_mention_emit() {
        let home = tempfile::TempDir::new().unwrap();
        let path = settings_path_in(home.path(), AgentName::Codex);
        let original = json!({
            "description": "keep this",
            "hooks": {"PermissionRequest": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "agent-witness emit"},
                {"type": "command", "command": "echo 'agent-witness emit'"}
            ]}]}
        });
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&original).unwrap()).unwrap();
        run_init_for_agent(&path, false, &clock(), AgentName::Codex).unwrap();
        let before_remove = std::fs::read(&path).unwrap();

        let removed =
            run_init_for_agent(&path, true, &FixedClock(TS + 1), AgentName::Codex).unwrap();

        assert_eq!(removed.outcome, InitOutcome::Removed);
        assert_eq!(
            std::fs::read(removed.backup.unwrap()).unwrap(),
            before_remove
        );
        assert_eq!(
            read_json(&path),
            json!({
                "description": "keep this",
                "hooks": {"PermissionRequest": [{"matcher": "Bash", "hooks": [
                    {"type": "command", "command": "echo 'agent-witness emit'"}
                ]}]}
            })
        );
        let again = run_init_for_agent(&path, true, &clock(), AgentName::Codex).unwrap();
        assert_eq!(again.outcome, InitOutcome::NothingToRemove);
        assert!(again.backup.is_none());
    }

    #[test]
    fn configured_agent_upgrade_preserves_existing_command_options_and_spacing() {
        for (command, expected) in [
            (
                "  agent-witness   emit --agent   claude-code --socket /tmp/test.sock",
                "  agent-witness   emit --agent   codex --socket /tmp/test.sock",
            ),
            (
                "agent-witness emit --agent=claude-code",
                "agent-witness emit --agent=codex",
            ),
            (
                "/tmp/emit/agent-witness emit --socket /tmp/test.sock",
                "/tmp/emit/agent-witness emit --agent codex --socket /tmp/test.sock",
            ),
            (
                "agent-witness emit --agent codex",
                "agent-witness emit --agent codex",
            ),
        ] {
            assert_eq!(command_with_agent(command, AgentName::Codex), expected);
        }
    }

    #[test]
    fn resolve_codex_settings_path_project_ends_in_codex_hooks() {
        let path = resolve_codex_settings_path(true).unwrap();
        assert!(path.ends_with(Path::new(CODEX_DIR).join(CODEX_HOOKS_FILE)));
    }
}
