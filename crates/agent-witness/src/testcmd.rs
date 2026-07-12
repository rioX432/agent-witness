//! Curated classification of test / build / lint shell commands (issue #63).
//!
//! Given a shell command recorded via Bash `tool_input.command`, decide whether
//! it is a **test-like** command and which kind. This is the evidence side of the
//! claim-vs-reality report (ADR-0005): it lets the report state, as a fact, that a
//! test/build/lint command was recorded — placed next to the agent's final
//! message so a human can do the contrast.
//!
//! Like [`crate::flags`], matching is **best-effort and token-based** over the
//! recorded command text — pure, dependency-free, deterministic. Each match
//! anchors on a segment's **leading command word**, so `echo "cargo test"` does
//! not classify (its leading token is `echo`). Coverage is a deliberately curated
//! allow-list: a runner not in the table is simply not classified — never guessed.
//! This is a documented gap, never a "we detect every test" claim.

use serde::Serialize;

/// The kind of a recognized test-like command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestKind {
    /// A test runner (`cargo test`, `pytest`, `npm test`, …).
    Test,
    /// A build / compile / typecheck step (`cargo build`, `go build`, `tsc`, …).
    Build,
    /// A linter / formatter / static check (`cargo clippy`, `eslint`, `ruff`, …).
    Lint,
}

impl TestKind {
    /// Lower-case label for rendering and JSON (matches the serde name).
    pub fn label(self) -> &'static str {
        match self {
            TestKind::Test => "test",
            TestKind::Build => "build",
            TestKind::Lint => "lint",
        }
    }

    /// Priority when a single command chains several kinds (`cargo build && cargo
    /// test`): `Test` is the strongest signal for "did the tests run?", so it
    /// wins, then `Build`, then `Lint`.
    fn rank(self) -> u8 {
        match self {
            TestKind::Test => 0,
            TestKind::Build => 1,
            TestKind::Lint => 2,
        }
    }
}

/// Classify a recorded command as test-like, or `None` if unrecognized.
///
/// The command is split into segments on `&&`, `||`, `;`, `|`, and newlines; each
/// segment is classified on its leading command word. When several segments match
/// (a chained command), the strongest kind wins (`Test` > `Build` > `Lint`), so a
/// `cargo build && cargo test` reads as a test command whose recorded status
/// covers the whole chain.
pub fn classify_command(command: &str) -> Option<TestKind> {
    let mut best: Option<TestKind> = None;
    for tokens in segments(command) {
        // Skip leading `NAME=value` env-assignments so classification anchors on
        // the command word (`RUST_LOG=debug cargo test` → `cargo test`), not the
        // assignment (issue #68).
        let start = tokens
            .iter()
            .position(|t| !is_env_assignment(t))
            .unwrap_or(tokens.len());
        let Some((&leading, args)) = tokens[start..].split_first() else {
            continue;
        };
        if let Some(kind) = classify_segment(leading, args) {
            best = Some(match best {
                Some(prev) if prev.rank() <= kind.rank() => prev,
                _ => kind,
            });
        }
    }
    best
}

/// Whether a token is a leading shell env-assignment (`NAME=value`) that precedes
/// the command word, as in `RUST_LOG=debug cargo test`. The name must be a shell
/// identifier (starts with a letter/`_`, then alphanumerics/`_`), so only genuine
/// assignments are skipped. Applied to leading tokens only, so a real argument
/// like `dd`'s `of=/dev/sda` elsewhere is never affected.
fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Split a command into segments on `&&`, `||`, `;`, `|`, and newlines, returning
/// each segment's whitespace-separated tokens. Mirrors the separator handling in
/// [`crate::flags`] (kept local: this side needs only the tokens, not the join
/// operator that the `curl … | sh` flag depends on).
fn segments(command: &str) -> Vec<Vec<&str>> {
    let bytes = command.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let sep_len = match bytes[i] {
            b'|' if bytes.get(i + 1) == Some(&b'|') => 2, // ||
            b'|' => 1,                                    // |
            b'&' if bytes.get(i + 1) == Some(&b'&') => 2, // &&
            b';' | b'\n' | b'\r' => 1,
            _ => 0,
        };
        if sep_len > 0 {
            out.push(command[start..i].split_ascii_whitespace().collect());
            i += sep_len;
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(command[start..].split_ascii_whitespace().collect());
    out
}

/// The first argument that is not an option flag (`-x` / `--long`), e.g. the
/// subcommand word in `cargo --quiet test`.
fn first_word<'a>(args: &[&'a str]) -> Option<&'a str> {
    args.iter().copied().find(|a| !a.starts_with('-'))
}

/// Classify one segment from its leading command word and its arguments.
fn classify_segment(leading: &str, args: &[&str]) -> Option<TestKind> {
    match leading {
        "cargo" => match first_word(args)? {
            "test" | "t" | "nextest" => Some(TestKind::Test),
            "build" | "b" | "check" | "c" => Some(TestKind::Build),
            "clippy" | "fmt" => Some(TestKind::Lint),
            _ => None,
        },
        // Recipe names are conventional; `verify`/`test` runners imply tests.
        "just" => match first_word(args)? {
            "test" | "t" | "verify" => Some(TestKind::Test),
            "check" | "lint" | "fmt" => Some(TestKind::Lint),
            "build" => Some(TestKind::Build),
            _ => None,
        },
        "npm" | "pnpm" | "yarn" | "bun" => classify_node_script(args),
        "go" => match first_word(args)? {
            "test" => Some(TestKind::Test),
            "build" => Some(TestKind::Build),
            "vet" => Some(TestKind::Lint),
            _ => None,
        },
        "make" => match first_word(args) {
            Some("test") => Some(TestKind::Test),
            Some("lint") => Some(TestKind::Lint),
            // Bare `make` or `make build` is a build step.
            None | Some("build") => Some(TestKind::Build),
            _ => None,
        },
        "dotnet" => match first_word(args)? {
            "test" => Some(TestKind::Test),
            "build" => Some(TestKind::Build),
            _ => None,
        },
        "mvn" | "gradle" | "gradlew" | "./gradlew" => classify_jvm(args),
        // Runners whose name is the command word.
        "pytest" | "py.test" | "tox" | "nose2" | "jest" | "vitest" | "mocha" | "ava" | "rspec"
        | "phpunit" => Some(TestKind::Test),
        "tsc" => Some(TestKind::Build),
        "eslint" | "ruff" | "flake8" | "pylint" | "golangci-lint" | "rubocop" => {
            Some(TestKind::Lint)
        }
        _ => None,
    }
}

/// `npm|pnpm|yarn|bun [run] <script>` — the script name after an optional `run`.
fn classify_node_script(args: &[&str]) -> Option<TestKind> {
    let script = match args.first().copied() {
        Some("run") => args.get(1).copied(),
        other => other,
    }?;
    match script {
        "test" | "t" => Some(TestKind::Test),
        "build" => Some(TestKind::Build),
        "lint" => Some(TestKind::Lint),
        _ => None,
    }
}

/// `mvn` / `gradle` goals — a `test` goal anywhere means tests; common build
/// goals mean build.
fn classify_jvm(args: &[&str]) -> Option<TestKind> {
    if args.contains(&"test") {
        Some(TestKind::Test)
    } else if args
        .iter()
        .any(|a| matches!(*a, "build" | "package" | "install" | "compile" | "assemble"))
    {
        Some(TestKind::Build)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_subcommands_classify_by_kind() {
        assert_eq!(
            classify_command("cargo test --workspace"),
            Some(TestKind::Test)
        );
        assert_eq!(classify_command("cargo nextest run"), Some(TestKind::Test));
        assert_eq!(
            classify_command("cargo build --release"),
            Some(TestKind::Build)
        );
        assert_eq!(classify_command("cargo check"), Some(TestKind::Build));
        assert_eq!(
            classify_command("cargo clippy -- -D warnings"),
            Some(TestKind::Lint)
        );
        assert_eq!(classify_command("cargo fmt --check"), Some(TestKind::Lint));
        assert_eq!(classify_command("cargo run"), None);
    }

    #[test]
    fn just_recipes_classify_by_convention() {
        assert_eq!(classify_command("just verify"), Some(TestKind::Test));
        assert_eq!(classify_command("just test"), Some(TestKind::Test));
        assert_eq!(classify_command("just check"), Some(TestKind::Lint));
        assert_eq!(classify_command("just build"), Some(TestKind::Build));
        assert_eq!(classify_command("just release"), None);
    }

    #[test]
    fn node_scripts_with_and_without_run() {
        assert_eq!(classify_command("npm test"), Some(TestKind::Test));
        assert_eq!(classify_command("npm run test"), Some(TestKind::Test));
        assert_eq!(classify_command("pnpm run build"), Some(TestKind::Build));
        assert_eq!(classify_command("yarn lint"), Some(TestKind::Lint));
        assert_eq!(classify_command("npm run dev"), None);
        assert_eq!(classify_command("npm install"), None);
    }

    #[test]
    fn standalone_runners_classify() {
        for cmd in ["pytest -q", "jest", "vitest run", "go test ./...", "rspec"] {
            assert_eq!(classify_command(cmd), Some(TestKind::Test), "for `{cmd}`");
        }
        assert_eq!(classify_command("tsc --noEmit"), Some(TestKind::Build));
        assert_eq!(classify_command("eslint ."), Some(TestKind::Lint));
        assert_eq!(classify_command("ruff check ."), Some(TestKind::Lint));
    }

    #[test]
    fn bare_make_is_build_but_make_test_is_test() {
        assert_eq!(classify_command("make"), Some(TestKind::Build));
        assert_eq!(classify_command("make build"), Some(TestKind::Build));
        assert_eq!(classify_command("make test"), Some(TestKind::Test));
        assert_eq!(classify_command("make lint"), Some(TestKind::Lint));
    }

    #[test]
    fn chained_command_takes_strongest_kind() {
        // Build + test in one line → test wins (the headline "did tests run?").
        assert_eq!(
            classify_command("cargo build && cargo test"),
            Some(TestKind::Test)
        );
        // cd prefix does not suppress the test segment (anchored per segment).
        assert_eq!(
            classify_command("cd crates/core && cargo test"),
            Some(TestKind::Test)
        );
        // Lint + build → build outranks lint.
        assert_eq!(
            classify_command("cargo clippy && cargo build"),
            Some(TestKind::Build)
        );
    }

    #[test]
    fn quoted_or_echoed_command_does_not_classify() {
        // Leading token is `echo`, so the inner text is not anchored.
        assert_eq!(classify_command("echo \"cargo test\""), None);
    }

    #[test]
    fn leading_env_assignments_are_skipped_before_anchoring() {
        // The regression from dogfooding: env-prefixed test runners (issue #68).
        assert_eq!(
            classify_command("UPDATE_GOLDEN=1 cargo nextest run"),
            Some(TestKind::Test)
        );
        assert_eq!(
            classify_command("RUST_LOG=debug cargo test"),
            Some(TestKind::Test)
        );
        assert_eq!(classify_command("CI=1 npm test"), Some(TestKind::Test));
        // Multiple assignments, and the kind still comes from the real command.
        assert_eq!(
            classify_command("FOO=1 BAR=2 cargo build"),
            Some(TestKind::Build)
        );
        // Env prefix inside a later segment of a chain.
        assert_eq!(
            classify_command("cd crates && RUSTFLAGS=-D cargo test"),
            Some(TestKind::Test)
        );
    }

    #[test]
    fn a_bare_env_assignment_is_not_a_command() {
        // No command word after the assignment(s) → nothing to classify.
        assert_eq!(classify_command("FOO=bar"), None);
        assert_eq!(classify_command("FOO=1 BAR=2"), None);
        // The echo protection still holds even with an env prefix.
        assert_eq!(classify_command("DEBUG=1 echo \"cargo test\""), None);
    }

    #[test]
    fn unrelated_commands_do_not_classify() {
        for cmd in [
            "ls -la",
            "git status",
            "grep -r foo .",
            "cat README.md",
            "cd src",
        ] {
            assert_eq!(classify_command(cmd), None, "for `{cmd}`");
        }
    }
}
