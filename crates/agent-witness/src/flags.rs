//! Curated, pattern-based flagging of destructive shell commands (issue #48).
//!
//! Given a shell command string recorded via Bash `tool_input.command`, this
//! module flags commands whose **class** is destructive (e.g. `rm -rf ~/`,
//! `git push --force`, `curl … | sh`), tags a severity, and describes the class.
//!
//! Honesty (Core Value 1 / ADR-0002) is the whole point of the framing here:
//!
//! - A flag means "this command's CLASS is destructive". It makes **no claim**
//!   about intent, maliciousness, outcome, or whether any damage occurred — a
//!   [`Flag::rationale`] describes the class only, never intent.
//! - Matching is **best-effort on the recorded command text**. Hooks cannot see
//!   the side effects inside `Bash("script.sh")`; quoting, heredocs, aliases,
//!   and obfuscation can evade the matcher. This is a documented coverage gap,
//!   never papered over — there is no "we catch everything" claim anywhere.
//!
//! The matcher is **token-based and dependency-free** (this repo deliberately
//! avoids adding dependencies; `regex` is only a transitive dep). It is a pure
//! function of its input — no wall-clock, no RNG — so callers stay deterministic.
//! [`crate::report`] (and the future digest) reuse [`flags_for_command`].

use serde::Serialize;

/// Severity of a raised [`Flag`]. Serialized `snake_case` (`critical` /
/// `warning`) to match the sibling enums in `agent-witness-core`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagSeverity {
    /// Irrecoverable / system-scope destruction (e.g. `rm -rf /`, `dd of=/dev`).
    Critical,
    /// Destructive but scoped/recoverable, or otherwise risky (e.g. force-push).
    Warning,
}

impl FlagSeverity {
    /// Sort key: critical rows sort before warning rows.
    pub fn rank(self) -> u8 {
        match self {
            Self::Critical => 0,
            Self::Warning => 1,
        }
    }

    /// Lower-case label for rendering (matches the serde name).
    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Warning => "warning",
        }
    }
}

/// One destructive-class match over a recorded command.
///
/// `rationale` describes the command **class** only (never intent or outcome);
/// `command` is the exact recorded command text the match was found in.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Flag {
    /// How severe the command class is.
    pub severity: FlagSeverity,
    /// Stable pattern id (e.g. `rm-rf-home-root`); handy for tooling/filtering.
    pub pattern: &'static str,
    /// Human-readable description of the command **class** — never intent.
    pub rationale: &'static str,
    /// The exact recorded command text this flag was raised for.
    pub command: String,
}

// --- pattern ids (stable, referenceable) -------------------------------------

const PATTERN_RM_RF_HOME_ROOT: &str = "rm-rf-home-root";
const PATTERN_DD_TO_DEVICE: &str = "dd-to-device";
const PATTERN_MKFS_DEVICE: &str = "mkfs-device";
const PATTERN_GIT_FORCE_PUSH: &str = "git-force-push";
const PATTERN_GIT_CLEAN_FORCE: &str = "git-clean-force";
const PATTERN_GIT_RESET_HARD: &str = "git-reset-hard";
const PATTERN_CHMOD_RECURSIVE_777: &str = "chmod-recursive-777";
const PATTERN_CURL_PIPE_SHELL: &str = "curl-pipe-shell";

// --- rationales (class descriptions, NOT intent) -----------------------------

const RATIONALE_RM_RF_HOME_ROOT: &str = "recursive force-remove targeting a home or root path";
const RATIONALE_DD_TO_DEVICE: &str = "dd writing directly to a block device";
const RATIONALE_MKFS_DEVICE: &str = "filesystem format on a device";
const RATIONALE_GIT_FORCE_PUSH: &str = "force-push rewrites remote history";
const RATIONALE_GIT_CLEAN_FORCE: &str = "git clean deletes untracked files";
const RATIONALE_GIT_RESET_HARD: &str = "git reset --hard discards uncommitted changes";
const RATIONALE_CHMOD_RECURSIVE_777: &str = "recursive chmod 777 removes permission boundaries";
const RATIONALE_CURL_PIPE_SHELL: &str = "remote script piped directly into a shell";

// --- pattern operands (named, no magic literals) -----------------------------

/// Targets that make an `rm -rf` catastrophic (home or root scope). Deliberately
/// narrow: scoped/relative targets like `~/project/build` or `./node_modules`
/// are NOT flagged, to keep the false-positive rate near zero (issue #48).
const RM_CATASTROPHIC_TARGETS: &[&str] =
    &["/", "~", "~/", "~/*", "$HOME", "${HOME}", "$HOME/", "/*"];

/// `dd`/`mkfs` operate on a raw block device when a target starts with this.
const DEVICE_PATH_PREFIX: &str = "/dev/";

/// `dd`'s output-file argument prefix.
const DD_OUTPUT_PREFIX: &str = "of=";

/// Mode tokens that make a recursive `chmod` world-writable/all-permissive.
const CHMOD_DANGEROUS_MODES: &[&str] = &["777", "a+rwx"];

/// Shells a piped remote script would execute in (`curl … | sh`).
const PIPE_TARGET_SHELLS: &[&str] = &["sh", "bash", "zsh"];

/// Downloaders whose output piped into a shell is the `curl | sh` class.
const REMOTE_FETCHERS: &[&str] = &["curl", "wget"];

// --- segment splitting -------------------------------------------------------

/// The operator connecting two pipeline/sequence segments. Only a plain single
/// pipe is distinguished, because one pattern (`curl … | sh`) spans it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sep {
    /// A single `|` (pipeline).
    Pipe,
    /// Any other separator: `&&`, `||`, `;`, or a newline.
    Other,
}

/// One pipeline/sequence segment plus the operator that joined it to the
/// previous segment (`None` for the first).
struct Segment<'a> {
    join: Option<Sep>,
    tokens: Vec<&'a str>,
}

impl<'a> Segment<'a> {
    fn leading(&self) -> Option<&'a str> {
        self.tokens.first().copied()
    }

    /// Tokens after the leading command word.
    fn args(&self) -> &[&'a str] {
        self.tokens.get(1..).unwrap_or(&[])
    }
}

/// Detect a shell separator at byte `i`. Returns the separator and its length.
/// All separators are ASCII, so byte indexing never lands inside a UTF-8 char.
fn separator_at(bytes: &[u8], i: usize) -> Option<(Sep, usize)> {
    match bytes[i] {
        b'|' => {
            if bytes.get(i + 1) == Some(&b'|') {
                Some((Sep::Other, 2)) // ||
            } else {
                Some((Sep::Pipe, 1)) // |
            }
        }
        b'&' if bytes.get(i + 1) == Some(&b'&') => Some((Sep::Other, 2)), // &&
        b';' | b'\n' | b'\r' => Some((Sep::Other, 1)),
        _ => None,
    }
}

/// Split a command into segments on `&&`, `||`, `;`, `|`, and newlines, keeping
/// the joining operator so the pipe-spanning pattern can see the `|` structure.
fn split_segments(command: &str) -> Vec<Segment<'_>> {
    let bytes = command.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut i = 0;
    // Operator that joins the segment currently being accumulated to the prior
    // one; `None` for the first segment.
    let mut incoming_join: Option<Sep> = None;

    while i < bytes.len() {
        if let Some((sep, len)) = separator_at(bytes, i) {
            segments.push(Segment {
                join: incoming_join,
                tokens: command[start..i].split_ascii_whitespace().collect(),
            });
            incoming_join = Some(sep);
            i += len;
            start = i;
        } else {
            i += 1;
        }
    }
    segments.push(Segment {
        join: incoming_join,
        tokens: command[start..].split_ascii_whitespace().collect(),
    });
    segments
}

// --- flag-combo helpers ------------------------------------------------------

/// Whether a token is a single-dash short-flag bundle containing `letter`
/// (folds `-rf` into `r` and `f`). Long flags (`--…`) are excluded.
fn short_flag_has(token: &str, letter: char) -> bool {
    match token.strip_prefix('-') {
        Some(rest) if !rest.starts_with('-') => rest.chars().any(|c| c == letter),
        _ => false,
    }
}

/// Recursive requested via `-r`/`-R` (bundled) or `--recursive`.
fn has_recursive(args: &[&str]) -> bool {
    args.iter()
        .any(|t| *t == "--recursive" || short_flag_has(t, 'r') || short_flag_has(t, 'R'))
}

/// Force requested via `-f` (bundled) or exactly `--force`. Note `--force`
/// matches exactly, so `--force-with-lease` is deliberately NOT treated as force.
fn has_force(args: &[&str]) -> bool {
    args.iter()
        .any(|t| *t == "--force" || short_flag_has(t, 'f'))
}

// --- the matcher -------------------------------------------------------------

/// Flags raised over one recorded command. May be empty, or carry several flags
/// (e.g. `git push -f && rm -rf ~/` raises two). Pure and deterministic.
///
/// Each flag anchors on a segment's **leading command token**, so text like
/// `echo "rm -rf ~/"` does not flag (the leading token is `echo`). Quoting is a
/// documented coverage gap — the matcher does not fully parse shell quoting.
pub fn flags_for_command(command: &str) -> Vec<Flag> {
    let segments = split_segments(command);
    let mut flags = Vec::new();

    for segment in &segments {
        let Some(leading) = segment.leading() else {
            continue;
        };
        let args = segment.args();
        match_rm(leading, args, command, &mut flags);
        match_dd(leading, args, command, &mut flags);
        match_mkfs(leading, args, command, &mut flags);
        match_git(leading, args, command, &mut flags);
        match_chmod(leading, args, command, &mut flags);
    }

    match_curl_pipe(&segments, command, &mut flags);
    flags
}

fn push_flag(
    flags: &mut Vec<Flag>,
    severity: FlagSeverity,
    pattern: &'static str,
    rationale: &'static str,
    command: &str,
) {
    flags.push(Flag {
        severity,
        pattern,
        rationale,
        command: command.to_string(),
    });
}

/// `rm -rf` targeting a home/root path (critical). Scoped/relative targets are
/// intentionally not flagged.
fn match_rm(leading: &str, args: &[&str], command: &str, flags: &mut Vec<Flag>) {
    if leading != "rm" {
        return;
    }
    if has_recursive(args)
        && has_force(args)
        && args.iter().any(|a| RM_CATASTROPHIC_TARGETS.contains(a))
    {
        push_flag(
            flags,
            FlagSeverity::Critical,
            PATTERN_RM_RF_HOME_ROOT,
            RATIONALE_RM_RF_HOME_ROOT,
            command,
        );
    }
}

/// `dd of=/dev/...` writing to a raw block device (critical).
fn match_dd(leading: &str, args: &[&str], command: &str, flags: &mut Vec<Flag>) {
    if leading != "dd" {
        return;
    }
    let writes_device = args.iter().any(|a| {
        a.strip_prefix(DD_OUTPUT_PREFIX)
            .is_some_and(|target| target.starts_with(DEVICE_PATH_PREFIX))
    });
    if writes_device {
        push_flag(
            flags,
            FlagSeverity::Critical,
            PATTERN_DD_TO_DEVICE,
            RATIONALE_DD_TO_DEVICE,
            command,
        );
    }
}

/// `mkfs` / `mkfs.<fs>` targeting a device (critical).
fn match_mkfs(leading: &str, args: &[&str], command: &str, flags: &mut Vec<Flag>) {
    let is_mkfs = leading == "mkfs" || leading.starts_with("mkfs.");
    if !is_mkfs {
        return;
    }
    if args.iter().any(|a| a.starts_with(DEVICE_PATH_PREFIX)) {
        push_flag(
            flags,
            FlagSeverity::Critical,
            PATTERN_MKFS_DEVICE,
            RATIONALE_MKFS_DEVICE,
            command,
        );
    }
}

/// `git` destructive subcommands: force-push, `clean -fd(x)`, `reset --hard`.
fn match_git(leading: &str, args: &[&str], command: &str, flags: &mut Vec<Flag>) {
    if leading != "git" {
        return;
    }
    let subcommand_args = |sub: &str| -> Option<&[&str]> {
        args.iter()
            .position(|a| *a == sub)
            .map(|pos| &args[pos + 1..])
    };

    // git push --force / -f — but NOT --force-with-lease.
    if let Some(push_args) = subcommand_args("push") {
        if has_force(push_args) {
            push_flag(
                flags,
                FlagSeverity::Warning,
                PATTERN_GIT_FORCE_PUSH,
                RATIONALE_GIT_FORCE_PUSH,
                command,
            );
        }
    }

    // git clean with force AND (-d or -x) — untracked-file deletion.
    if let Some(clean_args) = subcommand_args("clean") {
        let removes_dirs = clean_args
            .iter()
            .any(|t| short_flag_has(t, 'd') || short_flag_has(t, 'x'));
        if has_force(clean_args) && removes_dirs {
            push_flag(
                flags,
                FlagSeverity::Warning,
                PATTERN_GIT_CLEAN_FORCE,
                RATIONALE_GIT_CLEAN_FORCE,
                command,
            );
        }
    }

    // git reset --hard.
    if let Some(reset_args) = subcommand_args("reset") {
        if reset_args.contains(&"--hard") {
            push_flag(
                flags,
                FlagSeverity::Warning,
                PATTERN_GIT_RESET_HARD,
                RATIONALE_GIT_RESET_HARD,
                command,
            );
        }
    }
}

/// `chmod -R 777` (or `a+rwx`) recursively (warning). `chmod -R 755` is benign.
fn match_chmod(leading: &str, args: &[&str], command: &str, flags: &mut Vec<Flag>) {
    if leading != "chmod" {
        return;
    }
    let recursive = args
        .iter()
        .any(|t| *t == "--recursive" || short_flag_has(t, 'R'));
    let dangerous_mode = args.iter().any(|a| CHMOD_DANGEROUS_MODES.contains(a));
    if recursive && dangerous_mode {
        push_flag(
            flags,
            FlagSeverity::Warning,
            PATTERN_CHMOD_RECURSIVE_777,
            RATIONALE_CHMOD_RECURSIVE_777,
            command,
        );
    }
}

/// `curl`/`wget … | sh` — a remote fetcher piped directly into a shell (warning).
/// Spans the `|` split: a fetcher segment immediately followed, across a single
/// pipe, by a shell segment.
fn match_curl_pipe(segments: &[Segment<'_>], command: &str, flags: &mut Vec<Flag>) {
    for pair in segments.windows(2) {
        let (fetcher, shell) = (&pair[0], &pair[1]);
        let piped = shell.join == Some(Sep::Pipe);
        let from_fetcher = fetcher
            .leading()
            .is_some_and(|cmd| REMOTE_FETCHERS.contains(&cmd));
        let into_shell = shell
            .leading()
            .is_some_and(|cmd| PIPE_TARGET_SHELLS.contains(&cmd));
        if piped && from_fetcher && into_shell {
            push_flag(
                flags,
                FlagSeverity::Warning,
                PATTERN_CURL_PIPE_SHELL,
                RATIONALE_CURL_PIPE_SHELL,
                command,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert exactly one flag with the given pattern id and severity.
    fn assert_single(command: &str, pattern: &str, severity: FlagSeverity) {
        // Arrange / Act
        let flags = flags_for_command(command);
        // Assert
        assert_eq!(
            flags.len(),
            1,
            "expected one flag for `{command}`: {flags:?}"
        );
        assert_eq!(flags[0].pattern, pattern, "pattern for `{command}`");
        assert_eq!(flags[0].severity, severity, "severity for `{command}`");
        assert_eq!(flags[0].command, command, "verbatim command preserved");
    }

    fn assert_no_flag(command: &str) {
        let flags = flags_for_command(command);
        assert!(
            flags.is_empty(),
            "expected no flag for `{command}`: {flags:?}"
        );
    }

    // --- rm -rf home/root -----------------------------------------------------

    #[test]
    fn rm_rf_home_or_root_targets_flag_critical() {
        for command in [
            "rm -rf ~/",
            "rm -rf /",
            "rm -rf $HOME",
            "rm -fr ~",
            "rm --recursive --force /",
            "rm -rf ${HOME}",
            "rm -rf /*",
        ] {
            assert_single(command, PATTERN_RM_RF_HOME_ROOT, FlagSeverity::Critical);
        }
    }

    #[test]
    fn rm_rf_scoped_or_relative_targets_do_not_flag() {
        for command in [
            "rm -rf ./node_modules",
            "rm -rf node_modules",
            "rm -rf target",
            "rm -rf ~/project/build",
            "rm -rf dist/",
            "rm -rf", // no target at all
        ] {
            assert_no_flag(command);
        }
    }

    #[test]
    fn quoted_rm_with_leading_echo_does_not_flag() {
        // Leading token is `echo`, not `rm`, so anchoring suppresses this.
        assert_no_flag("echo \"rm -rf ~/\"");
    }

    // --- dd / mkfs ------------------------------------------------------------

    #[test]
    fn dd_to_device_flags_critical_but_file_output_does_not() {
        assert_single(
            "dd if=x of=/dev/sda",
            PATTERN_DD_TO_DEVICE,
            FlagSeverity::Critical,
        );
        assert_no_flag("dd if=/dev/zero of=./disk.img");
    }

    #[test]
    fn mkfs_on_device_flags_critical() {
        assert_single(
            "mkfs.ext4 /dev/sdb1",
            PATTERN_MKFS_DEVICE,
            FlagSeverity::Critical,
        );
        assert_single(
            "mkfs /dev/nvme0n1",
            PATTERN_MKFS_DEVICE,
            FlagSeverity::Critical,
        );
        assert_no_flag("mkfs.ext4 disk.img");
    }

    // --- git push -------------------------------------------------------------

    #[test]
    fn git_force_push_flags_warning() {
        assert_single("git push -f", PATTERN_GIT_FORCE_PUSH, FlagSeverity::Warning);
        assert_single(
            "git push --force",
            PATTERN_GIT_FORCE_PUSH,
            FlagSeverity::Warning,
        );
    }

    #[test]
    fn git_safe_push_variants_do_not_flag() {
        assert_no_flag("git push origin main");
        // The safe force variant must never flag.
        assert_no_flag("git push --force-with-lease origin feat");
    }

    // --- git clean ------------------------------------------------------------

    #[test]
    fn git_clean_force_with_dirs_flags_warning() {
        for command in [
            "git clean -fd",
            "git clean -fdx",
            "git clean -fx",
            "git clean -f -d",
        ] {
            assert_single(command, PATTERN_GIT_CLEAN_FORCE, FlagSeverity::Warning);
        }
    }

    #[test]
    fn git_clean_dry_run_does_not_flag() {
        assert_no_flag("git clean -n");
    }

    // --- git reset ------------------------------------------------------------

    #[test]
    fn git_reset_hard_flags_warning() {
        assert_single(
            "git reset --hard",
            PATTERN_GIT_RESET_HARD,
            FlagSeverity::Warning,
        );
        assert_single(
            "git reset --hard HEAD~1",
            PATTERN_GIT_RESET_HARD,
            FlagSeverity::Warning,
        );
    }

    #[test]
    fn git_reset_soft_does_not_flag() {
        assert_no_flag("git reset --soft HEAD~1");
    }

    // --- chmod ----------------------------------------------------------------

    #[test]
    fn chmod_recursive_777_flags_warning() {
        assert_single(
            "chmod -R 777 .",
            PATTERN_CHMOD_RECURSIVE_777,
            FlagSeverity::Warning,
        );
        assert_single(
            "chmod --recursive 777 x",
            PATTERN_CHMOD_RECURSIVE_777,
            FlagSeverity::Warning,
        );
        assert_single(
            "chmod -R a+rwx .",
            PATTERN_CHMOD_RECURSIVE_777,
            FlagSeverity::Warning,
        );
    }

    #[test]
    fn chmod_recursive_755_does_not_flag() {
        assert_no_flag("chmod -R 755 dir");
    }

    // --- curl | sh ------------------------------------------------------------

    #[test]
    fn remote_fetch_piped_into_shell_flags_warning() {
        assert_single(
            "curl -fsSL https://x | sh",
            PATTERN_CURL_PIPE_SHELL,
            FlagSeverity::Warning,
        );
        assert_single(
            "wget -qO- https://x | bash",
            PATTERN_CURL_PIPE_SHELL,
            FlagSeverity::Warning,
        );
    }

    #[test]
    fn curl_without_shell_pipe_does_not_flag() {
        assert_no_flag("curl -o out.tgz https://x");
    }

    // --- multi-segment --------------------------------------------------------

    #[test]
    fn multi_segment_raises_a_flag_per_matching_segment() {
        // Arrange / Act
        let flags = flags_for_command("git push -f && rm -rf ~/");
        // Assert: two flags, one per segment, both carrying the full command text.
        assert_eq!(flags.len(), 2, "{flags:?}");
        let patterns: Vec<&str> = flags.iter().map(|f| f.pattern).collect();
        assert!(patterns.contains(&PATTERN_GIT_FORCE_PUSH));
        assert!(patterns.contains(&PATTERN_RM_RF_HOME_ROOT));
        assert!(flags
            .iter()
            .all(|f| f.command == "git push -f && rm -rf ~/"));
    }

    #[test]
    fn newline_and_semicolon_separators_anchor_each_segment() {
        // A newline (and `;`) splits segments, so the destructive one anchors on
        // its own leading token rather than the harmless first command.
        assert_single(
            "echo hi\nrm -rf ~/",
            PATTERN_RM_RF_HOME_ROOT,
            FlagSeverity::Critical,
        );
        assert_single(
            "cd /tmp; rm -rf /",
            PATTERN_RM_RF_HOME_ROOT,
            FlagSeverity::Critical,
        );
    }

    #[test]
    fn severity_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&FlagSeverity::Critical).unwrap(),
            "\"critical\""
        );
        assert_eq!(
            serde_json::to_string(&FlagSeverity::Warning).unwrap(),
            "\"warning\""
        );
    }
}
