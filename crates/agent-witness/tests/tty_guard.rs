//! TUI commands must fail cleanly, not panic, when stdout is not a TTY
//! (issue #40). These tests run the real binary with piped stdio — exactly the
//! environment (CI, pipes, editor-embedded shells) the guard exists for.

use std::process::{Command, Stdio};

/// Path of the binary under test, provided by cargo for integration tests.
const BIN: &str = env!("CARGO_BIN_EXE_agent-witness");

/// Run `agent-witness <args>` with piped stdio and a tempdir HOME, returning
/// (exit status code, stderr). A tempdir HOME keeps the store empty and the
/// real one untouched.
fn run_piped(args: &[&str]) -> (Option<i32>, String) {
    let home = tempfile::TempDir::new().expect("tempdir");
    let output = Command::new(BIN)
        .args(args)
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("binary runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn top_without_tty_fails_cleanly_and_suggests_ls_live() {
    let (code, stderr) = run_piped(&["top"]);
    assert_eq!(code, Some(1), "clean error exit, not a panic (101)");
    assert!(
        stderr.contains("needs an interactive terminal"),
        "actionable message, got: {stderr}"
    );
    assert!(stderr.contains("ls --live"), "suggests the fallback");
    assert!(!stderr.contains("panicked"), "must not panic: {stderr}");
}

#[test]
fn show_without_tty_fails_cleanly_and_suggests_report() {
    let (code, stderr) = run_piped(&["show"]);
    assert_eq!(code, Some(1), "clean error exit, not a panic (101)");
    assert!(
        stderr.contains("needs an interactive terminal"),
        "actionable message, got: {stderr}"
    );
    assert!(stderr.contains("report"), "suggests the fallback");
    assert!(!stderr.contains("panicked"), "must not panic: {stderr}");
}
