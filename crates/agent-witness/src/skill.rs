//! `agent-witness init`: installation of the `/witness` skill, the
//! conversational companion to the recording hooks.
//!
//! The skill lets a Claude Code session answer "what did my last session do?"
//! by running `agent-witness report` itself, so the audit record is reachable
//! without leaving the conversation. Like the hooks edit in [`crate::init`],
//! writing into `~/.claude` follows a strict contract:
//!
//! - **Never clobber a foreign file.** Our file carries [`OWN_MARKER`]; a file
//!   at our path without it is the user's and is left untouched (reported, not
//!   overwritten). Symlinks and non-UTF-8 files are foreign by definition — we
//!   only ever manage a regular UTF-8 file we wrote ourselves, and writing
//!   through a symlink could land content outside `.claude`.
//! - **Idempotent.** Re-running `init` with identical content changes nothing.
//! - **Backed up.** A managed file whose content differs (an older version, or
//!   local edits) is copied to `SKILL.md.bak-<ts>` before being rewritten or
//!   removed.
//! - **`--remove` is surgical.** It deletes only our file and prunes the then-
//!   empty `witness/` directory — never `skills/` or `.claude/` themselves.
//!
//! The skill path is passed in explicitly so tests run against a tempdir and
//! never touch the real home directory.

use std::path::{Path, PathBuf};

use agent_witness_core::Clock;
use anyhow::{anyhow, Context, Result};

use crate::{backup, paths};

/// Substring identifying the skill file as ours; checked before any overwrite
/// or removal so a user's own skill at the same path is never touched.
const OWN_MARKER: &str = "managed by agent-witness init";

/// Directory under home / project root holding Claude Code configuration.
const CLAUDE_DIR: &str = ".claude";
/// Skills sub-directory.
const SKILLS_DIR: &str = "skills";
/// Our skill's directory name (the skill is invoked as `/witness`).
const SKILL_NAME: &str = "witness";
/// Skill definition file name expected by Claude Code.
const SKILL_FILE: &str = "SKILL.md";

/// The full `/witness` skill definition installed by `init`.
///
/// The trust-boundary section is a hard requirement (issue #30): the skill is a
/// convenience view, and must say so — an agent narrating its own audit trail
/// is not evidence for incident review.
const SKILL_CONTENT: &str = r#"---
name: witness
description: "Query the local agent-witness audit record: summarize what a recorded agent session actually did (tool calls, files, commands) by running the agent-witness CLI"
argument-hint: "[session selector: @last, @2, @project:<name>, @live:1, id prefix — or empty for this directory's latest]"
user-invocable: true
allowed-tools:
  - Bash(agent-witness report:*)
  - Bash(agent-witness ls:*)
  - Bash(agent-witness digest:*)
  - Bash(agent-witness inventory:*)
---

<!-- managed by agent-witness init; re-running `agent-witness init` may overwrite
     this file (a timestamped backup is kept). Remove it with
     `agent-witness init --remove`. -->

# /witness — query the agent-witness audit record

Answer the user's question about what a recorded agent session actually did by
querying the local agent-witness store. Every claim must come from command
output — never from memory of the conversation.

**Arguments:** $ARGUMENTS

## How

1. Run `agent-witness report $ARGUMENTS` (with no argument it reports the
   latest session recorded from the current directory). Add `--json` when you
   need to count or aggregate rather than quote.
2. To find a session first, run `agent-witness ls` and pick a selector
   (`@last`, `@2`, `@project:<substring>`, `@live:1`, or an id prefix).
3. Summarize honestly: which tools ran, which files were touched, which
   commands executed. Keep each event's attribution (`direct | observed |
   inferred`) visible when it matters — never present an `inferred` event as an
   observed fact.

## Usage audit

When asked to audit AI usage — wasteful loops, model choices, per-project
delegation — aggregate the record instead of quoting a single session:

1. Run `agent-witness digest` (`--week`, `--today`, or `--since <dur>`; add
   `--json` to aggregate) for a cross-session, per-project ledger: sessions,
   durations, tool calls, commands, destructive-class command-flag counts, and
   per-model token totals.
2. Run `agent-witness inventory` for the capability surface — which configured
   MCP servers and skills were actually used versus merely configured.
3. Summarize HONESTLY. The CLI gives FACTS ONLY; any "wasteful", "oversized", or
   "risky" characterization is YOUR interpretation — say so, and never present it
   as a CLI claim. A missing usage sidecar means "usage unavailable", not zero
   tokens.

For an unattended, scheduled weekly version of this audit, see
`docs/recipes/scheduled-audit.md`.

## Trust boundary

This skill is a convenience view, **not the trusted read path**. The agent
summarizing the record here is the same kind of agent the record observes. For
incident review — e.g. suspected prompt injection — do not rely on an agent's
narration of its own audit trail: open the record directly in your own
terminal with `agent-witness show <selector>` or `agent-witness report
<selector>`.
"#;

/// What the skill step of `init` did — surfaced for the CLI report and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillOutcome {
    /// The skill file was created.
    Installed,
    /// A managed file with different content was rewritten (backup kept).
    Updated,
    /// The current content was already in place; nothing changed.
    AlreadyInstalled,
    /// Our skill file was deleted.
    Removed,
    /// No file of ours was present to remove; nothing changed.
    NothingToRemove,
    /// A file without our marker occupies the path; it was left untouched.
    SkippedForeign,
}

/// Result of the skill step: what changed and the backup file (if one was made).
#[derive(Debug, Clone)]
pub struct SkillReport {
    /// Outcome of the run.
    pub outcome: SkillOutcome,
    /// Path of the backup written before rewriting or deleting a modified
    /// managed file, if any.
    pub backup: Option<PathBuf>,
}

/// Resolve the skill file to manage: `~/.claude/skills/witness/SKILL.md`, or
/// the same path under the current directory when `project` is set.
pub fn resolve_skill_path(project: bool) -> Result<PathBuf> {
    let base = if project {
        std::env::current_dir().context("cannot determine current directory")?
    } else {
        paths::home_dir()?
    };
    Ok(base
        .join(CLAUDE_DIR)
        .join(SKILLS_DIR)
        .join(SKILL_NAME)
        .join(SKILL_FILE))
}

/// Classification of whatever currently sits at the skill path. Foreign covers
/// everything we did not verifiably write: no marker, non-UTF-8 content, or a
/// symlink (following one on write could land content outside `.claude`).
enum Existing {
    /// Nothing at the path.
    Absent,
    /// A regular UTF-8 file carrying [`OWN_MARKER`] — ours to manage.
    Managed(String),
    /// Anything else — never touched.
    Foreign,
}

/// Install (or, with `remove`, uninstall) the `/witness` skill at `skill_path`.
///
/// Refuses to touch anything classified [`Existing::Foreign`], and backs up a
/// managed file whose content differs before rewriting or deleting it.
pub fn run_skill(skill_path: &Path, remove: bool, clock: &dyn Clock) -> Result<SkillReport> {
    let existing = classify_existing(skill_path)?;

    if matches!(existing, Existing::Foreign) {
        return Ok(SkillReport {
            outcome: SkillOutcome::SkippedForeign,
            backup: None,
        });
    }

    if remove {
        return match existing {
            Existing::Absent => Ok(SkillReport {
                outcome: SkillOutcome::NothingToRemove,
                backup: None,
            }),
            Existing::Managed(content) => {
                // Locally edited managed files are preserved as a backup; a
                // pristine copy of our own content is re-creatable and is not.
                let backup = if content == SKILL_CONTENT {
                    None
                } else {
                    Some(backup::write_backup(skill_path, &content, clock.now_ms())?)
                };
                std::fs::remove_file(skill_path)
                    .with_context(|| format!("cannot remove {}", skill_path.display()))?;
                prune_skill_dir(skill_path);
                Ok(SkillReport {
                    outcome: SkillOutcome::Removed,
                    backup,
                })
            }
            Existing::Foreign => unreachable!("foreign handled above"),
        };
    }

    match existing {
        Existing::Managed(content) if content == SKILL_CONTENT => Ok(SkillReport {
            outcome: SkillOutcome::AlreadyInstalled,
            backup: None,
        }),
        Existing::Managed(content) => {
            let backup = backup::write_backup(skill_path, &content, clock.now_ms())?;
            write_skill(skill_path)?;
            Ok(SkillReport {
                outcome: SkillOutcome::Updated,
                backup: Some(backup),
            })
        }
        Existing::Absent => {
            write_skill(skill_path)?;
            Ok(SkillReport {
                outcome: SkillOutcome::Installed,
                backup: None,
            })
        }
        Existing::Foreign => unreachable!("foreign handled above"),
    }
}

/// Classify what currently sits at the skill path.
///
/// The symlink check runs first so a dangling symlink reads as foreign rather
/// than absent — `fs::write` would follow it and create the file at its target.
fn classify_existing(path: &Path) -> Result<Existing> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Existing::Absent),
        Err(e) => return Err(anyhow!("cannot stat {}: {e}", path.display())),
        Ok(meta) if meta.file_type().is_symlink() => return Ok(Existing::Foreign),
        Ok(_) => {}
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Existing::Absent),
        Err(e) => return Err(anyhow!("cannot read {}: {e}", path.display())),
    };
    match String::from_utf8(bytes) {
        Ok(content) if content.contains(OWN_MARKER) => Ok(Existing::Managed(content)),
        _ => Ok(Existing::Foreign),
    }
}

/// Write the skill definition, creating parent directories as needed.
fn write_skill(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    std::fs::write(path, SKILL_CONTENT).with_context(|| format!("cannot write {}", path.display()))
}

/// Best-effort removal of the now-empty `witness/` directory after `--remove`.
/// Fails silently when the directory still holds other files (e.g. backups) —
/// pruning is cosmetic and must never delete anything but an empty directory.
fn prune_skill_dir(skill_path: &Path) {
    if let Some(dir) = skill_path.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_witness_core::FixedClock;

    const TS: i64 = 1_700_000_000_000;

    fn clock() -> FixedClock {
        FixedClock(TS)
    }

    fn skill_path(tmp: &tempfile::TempDir) -> PathBuf {
        tmp.path()
            .join(".claude")
            .join("skills")
            .join("witness")
            .join("SKILL.md")
    }

    #[test]
    fn content_carries_marker_and_trust_boundary() {
        // The marker is what makes every ownership check in this module work,
        // and the trust-boundary section is a hard requirement of issue #30.
        assert!(SKILL_CONTENT.contains(OWN_MARKER));
        assert!(SKILL_CONTENT.contains("Trust boundary"));
        assert!(SKILL_CONTENT.contains("not the trusted read path"));
    }

    #[test]
    fn content_has_usage_audit_section_and_allows_audit_commands() {
        // The audit flow (issue #49) must be present, and the two aggregation
        // commands it drives must be pre-authorized in the frontmatter — while
        // the original report/ls grants stay intact.
        assert!(SKILL_CONTENT.contains("## Usage audit"));
        assert!(SKILL_CONTENT.contains("Bash(agent-witness report:*)"));
        assert!(SKILL_CONTENT.contains("Bash(agent-witness ls:*)"));
        assert!(SKILL_CONTENT.contains("Bash(agent-witness digest:*)"));
        assert!(SKILL_CONTENT.contains("Bash(agent-witness inventory:*)"));
        // The facts-vs-judgment boundary must be explicit, not implied: the CLI
        // gives facts, the characterization is the agent's own.
        assert!(SKILL_CONTENT.contains("FACTS ONLY"));
        assert!(SKILL_CONTENT.contains("usage unavailable"));
    }

    #[test]
    fn install_creates_file_and_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = skill_path(&tmp);

        let report = run_skill(&path, false, &clock()).unwrap();
        assert_eq!(report.outcome, SkillOutcome::Installed);
        assert!(report.backup.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL_CONTENT);

        let again = run_skill(&path, false, &clock()).unwrap();
        assert_eq!(again.outcome, SkillOutcome::AlreadyInstalled);
        assert!(again.backup.is_none());
    }

    #[test]
    fn install_updates_outdated_managed_file_with_backup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = skill_path(&tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old = format!("old version, {OWN_MARKER}\n");
        std::fs::write(&path, &old).unwrap();

        let report = run_skill(&path, false, &clock()).unwrap();
        assert_eq!(report.outcome, SkillOutcome::Updated);
        let backup = report.backup.expect("outdated content must be backed up");
        assert_eq!(backup, crate::backup::backup_path(&path, TS));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), old);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SKILL_CONTENT);
    }

    #[test]
    fn symlink_at_skill_path_is_foreign_even_when_dangling() {
        // fs::write follows symlinks, so writing through one could create the
        // file at the link's target — outside .claude. Both a dangling link
        // (which plain read would misread as "absent") and one pointing at our
        // own content must be classified foreign and left alone.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = skill_path(&tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let target = tmp.path().join("elsewhere.md");
        std::os::unix::fs::symlink(&target, &path).unwrap();

        let report = run_skill(&path, false, &clock()).unwrap();
        assert_eq!(report.outcome, SkillOutcome::SkippedForeign);
        assert!(!target.exists(), "must not write through the symlink");

        let removed = run_skill(&path, true, &clock()).unwrap();
        assert_eq!(removed.outcome, SkillOutcome::SkippedForeign);
        assert!(
            std::fs::symlink_metadata(&path).is_ok(),
            "symlink itself must survive"
        );
    }

    #[test]
    fn non_utf8_file_is_foreign_not_an_error() {
        // A foreign binary/latin-1 file must not abort init (the hooks step has
        // already run by the time the skill step executes) — it is simply not
        // ours and is skipped.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = skill_path(&tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = [0xC3u8, 0x28, 0xFF, 0xFE];
        std::fs::write(&path, bytes).unwrap();

        let report = run_skill(&path, false, &clock()).unwrap();
        assert_eq!(report.outcome, SkillOutcome::SkippedForeign);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn install_never_touches_foreign_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = skill_path(&tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let foreign = "---\nname: witness\n---\nthe user's own skill\n";
        std::fs::write(&path, foreign).unwrap();

        let report = run_skill(&path, false, &clock()).unwrap();
        assert_eq!(report.outcome, SkillOutcome::SkippedForeign);
        assert!(report.backup.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), foreign);
    }

    #[test]
    fn remove_deletes_pristine_file_without_backup_and_prunes_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = skill_path(&tmp);
        run_skill(&path, false, &clock()).unwrap();

        let report = run_skill(&path, true, &clock()).unwrap();
        assert_eq!(report.outcome, SkillOutcome::Removed);
        assert!(
            report.backup.is_none(),
            "pristine content is re-creatable; no backup needed"
        );
        assert!(!path.exists());
        assert!(
            !path.parent().unwrap().exists(),
            "empty witness/ dir should be pruned"
        );
        assert!(
            path.parent().unwrap().parent().unwrap().exists(),
            "skills/ itself must survive"
        );
    }

    #[test]
    fn remove_backs_up_locally_edited_managed_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = skill_path(&tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let edited = format!("{SKILL_CONTENT}\nuser tweak\n");
        std::fs::write(&path, &edited).unwrap();

        let report = run_skill(&path, true, &clock()).unwrap();
        assert_eq!(report.outcome, SkillOutcome::Removed);
        let backup = report.backup.expect("edited content must be backed up");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), edited);
        assert!(!path.exists());
        assert!(
            path.parent().unwrap().exists(),
            "dir with the backup in it must not be pruned"
        );
    }

    #[test]
    fn remove_never_touches_foreign_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = skill_path(&tmp);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let foreign = "the user's own skill\n";
        std::fs::write(&path, foreign).unwrap();

        let report = run_skill(&path, true, &clock()).unwrap();
        assert_eq!(report.outcome, SkillOutcome::SkippedForeign);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), foreign);
    }

    #[test]
    fn remove_with_no_file_is_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = skill_path(&tmp);
        let report = run_skill(&path, true, &clock()).unwrap();
        assert_eq!(report.outcome, SkillOutcome::NothingToRemove);
        assert!(!path.exists(), "must not create a file on remove");
    }

    #[test]
    fn resolve_skill_path_project_ends_in_skill_file() {
        let path = resolve_skill_path(true).unwrap();
        assert!(path.ends_with(
            Path::new(CLAUDE_DIR)
                .join(SKILLS_DIR)
                .join(SKILL_NAME)
                .join(SKILL_FILE)
        ));
    }
}
