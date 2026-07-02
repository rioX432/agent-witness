//! Golden-fixture lint (issue #8).
//!
//! Loads every `tests/fixtures/<scenario>/hooks.jsonl`, validates each line as a
//! hook payload with the expected fields, and mechanically asserts no real
//! paths / usernames / secrets / raw ids leaked through sanitization.
//!
//! Two roles:
//! 1. Sanitization gate — a leaked path or secret is a Critical bug (ADR-0002).
//! 2. Canary — the per-event required-field checks fail if an upstream Claude
//!    Code release renames or drops a hook field, surfacing a payload-shape diff.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Locate the repo-root `tests/fixtures` dir relative to this crate.
fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
}

/// Scenario dirs that contain a `hooks.jsonl`, sorted for determinism.
fn scenario_files() -> Vec<(String, PathBuf)> {
    let root = fixtures_root();
    let mut out = Vec::new();
    for entry in fs::read_dir(&root).expect("read tests/fixtures") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            let hooks = path.join("hooks.jsonl");
            if hooks.is_file() {
                let name = entry.file_name().to_string_lossy().into_owned();
                out.push((name, hooks));
            }
        }
    }
    out.sort();
    out
}

fn load_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

// --- mechanical scanners (regex-free, byte-safe for UTF-8 payloads) ----------

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

/// Whole-word (ASCII-boundary) containment, so masking `user` does not trip on
/// substrings like `superuser`.
fn contains_word(haystack: &str, word: &str) -> bool {
    let (hb, wb) = (haystack.as_bytes(), word.as_bytes());
    if wb.is_empty() || hb.len() < wb.len() {
        return false;
    }
    for start in 0..=hb.len() - wb.len() {
        if &hb[start..start + wb.len()] == wb {
            let before = start == 0 || !is_word_byte(hb[start - 1]);
            let after_i = start + wb.len();
            let after = after_i == hb.len() || !is_word_byte(hb[after_i]);
            if before && after {
                return true;
            }
        }
    }
    false
}

/// Detect a raw `8-4-4-4-12` hex UUID (sanitizer replaces these with
/// non-hex placeholders like `session-basic` / `uuid-0001`).
fn contains_hex_uuid(s: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    const TOTAL: usize = 36; // 32 hex + 4 hyphens
    let b = s.as_bytes();
    if b.len() < TOTAL {
        return false;
    }
    'start: for start in 0..=b.len() - TOTAL {
        let mut i = start;
        for (gi, &g) in GROUPS.iter().enumerate() {
            for _ in 0..g {
                if !b[i].is_ascii_hexdigit() {
                    continue 'start;
                }
                i += 1;
            }
            if gi + 1 < GROUPS.len() {
                if b[i] != b'-' {
                    continue 'start;
                }
                i += 1;
            }
        }
        return true;
    }
    false
}

/// Detect a raw tool_use_id (`toolu_` + base62). The sanitizer maps these to
/// all-digit placeholders, so any alphabetic char in the trailing run is a leak.
fn contains_raw_toolu_id(s: &str) -> bool {
    let b = s.as_bytes();
    let needle = b"toolu_";
    if b.len() < needle.len() {
        return false;
    }
    for start in 0..=b.len() - needle.len() {
        if &b[start..start + needle.len()] == needle {
            let mut i = start + needle.len();
            while i < b.len() && is_word_byte(b[i]) {
                if b[i].is_ascii_alphabetic() {
                    return true;
                }
                i += 1;
            }
        }
    }
    false
}

/// Detect a `/home/<name>` path whose first segment is anything other than the
/// sanitizer's `user` placeholder. Catches leaked Linux home paths (and thus
/// path-embedded usernames) regardless of which machine runs this test.
fn contains_non_placeholder_home(s: &str) -> bool {
    const PREFIX: &str = "/home/";
    const PLACEHOLDER: &str = "user";
    let mut rest = s;
    while let Some(pos) = rest.find(PREFIX) {
        let after = &rest[pos + PREFIX.len()..];
        let segment_end = after
            .find(|c: char| c == '/' || c.is_whitespace() || c == '"' || c == '\'' || c == '\\')
            .unwrap_or(after.len());
        let segment = &after[..segment_end];
        if !segment.is_empty() && segment != PLACEHOLDER {
            return true;
        }
        rest = after;
    }
    false
}

/// Substrings that must never appear (paths + known secret/token prefixes).
const FORBIDDEN_SUBSTRINGS: &[&str] = &[
    "/Users/",
    "/private/tmp",
    "/var/folders",
    "sk-ant-",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "AKIA",
    "xoxb-",
];

/// Real identity tokens derived from the current environment (no hardcoded
/// names): the machine that captures/edits fixtures must not leak its own
/// `$USER` or `$HOME` (task requirement: "no real $USER strings").
///
/// Known limitation: this only sees the *current* runner's identity, so it is
/// strongest when run on the capture machine (`just verify` before commit —
/// the documented workflow). On CI it degrades to a no-op for the capturer's
/// name; path-embedded usernames are still caught machine-independently by
/// `contains_non_placeholder_home` and the `/Users/` forbidden substring.
fn env_identity_tokens() -> Vec<String> {
    // The sanitizer's placeholder; must not be treated as a real identity, or a
    // machine whose real user is literally `user` would trip on `/home/user`.
    const PLACEHOLDER: &str = "user";
    let mut tokens = Vec::new();
    if let Ok(user) = std::env::var("USER") {
        if user.len() >= 2 && user != PLACEHOLDER {
            tokens.push(user);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if let Some(base) = Path::new(&home).file_name() {
            let base = base.to_string_lossy().into_owned();
            if base.len() >= 2 && base != PLACEHOLDER {
                tokens.push(base);
            }
        }
    }
    tokens
}

#[test]
fn required_scenarios_present() {
    let names: Vec<String> = scenario_files().into_iter().map(|(n, _)| n).collect();
    for required in ["session-basic", "session-with-failure"] {
        assert!(
            names.iter().any(|n| n == required),
            "missing required fixture scenario `{required}`; found {names:?}"
        );
    }
    assert!(
        names.len() >= 2,
        "expected at least 2 scenarios, got {names:?}"
    );
}

#[test]
fn every_line_is_a_json_object() {
    for (scenario, path) in scenario_files() {
        let lines = load_lines(&path);
        assert!(!lines.is_empty(), "{scenario}: no events");
        for (i, line) in lines.iter().enumerate() {
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("{scenario} line {}: invalid JSON: {e}", i + 1));
            assert!(
                value.is_object(),
                "{scenario} line {}: expected a JSON object",
                i + 1
            );
        }
    }
}

/// Canary: expected field set per hook event. Renames/removals upstream fail here.
#[test]
fn hook_payloads_have_expected_fields() {
    // Fields required on every hook payload.
    let common = ["hook_event_name", "session_id", "cwd", "transcript_path"];
    for (scenario, path) in scenario_files() {
        for (i, line) in load_lines(&path).iter().enumerate() {
            let ln = i + 1;
            let obj: Value = serde_json::from_str(line).unwrap();
            for field in common {
                assert!(
                    obj.get(field).and_then(Value::as_str).is_some(),
                    "{scenario} line {ln}: missing string field `{field}`"
                );
            }
            let event = obj["hook_event_name"].as_str().unwrap();
            let extra: &[&str] = match event {
                "PreToolUse" => &["tool_name", "tool_input", "tool_use_id"],
                "PostToolUse" => &["tool_name", "tool_input", "tool_response", "tool_use_id"],
                "UserPromptSubmit" => &["prompt"],
                "Stop" => &["stop_hook_active"],
                "SessionStart" => &["source"],
                _ => &[],
            };
            for field in extra {
                assert!(
                    obj.get(*field).is_some(),
                    "{scenario} line {ln} ({event}): missing field `{field}`"
                );
            }
        }
    }
}

/// The sanitization gate: no real paths, usernames, secrets, or raw ids.
#[test]
fn fixtures_contain_no_unsanitized_data() {
    let identity = env_identity_tokens();
    for (scenario, path) in scenario_files() {
        for (i, line) in load_lines(&path).iter().enumerate() {
            let ln = i + 1;
            for bad in FORBIDDEN_SUBSTRINGS {
                assert!(
                    !line.contains(bad),
                    "{scenario} line {ln}: forbidden substring {bad:?} (unsanitized data)"
                );
            }
            for token in &identity {
                assert!(
                    !contains_word(line, token),
                    "{scenario} line {ln}: leaked environment identity token {token:?}"
                );
            }
            assert!(
                !contains_non_placeholder_home(line),
                "{scenario} line {ln}: non-placeholder /home/<name> path (leaked username)"
            );
            assert!(
                !contains_hex_uuid(line),
                "{scenario} line {ln}: raw hex UUID left unsanitized"
            );
            assert!(
                !contains_raw_toolu_id(line),
                "{scenario} line {ln}: raw tool_use_id left unsanitized"
            );
        }
    }
}

/// Canary + honest-observation check: in the failure scenario a failed Bash tool
/// call fires a `PreToolUse` with no matching `PostToolUse` (Claude Code
/// 2.1.198). If upstream starts emitting PostToolUse on failure, this fails —
/// which is the signal to re-inspect the fixtures.
#[test]
fn failure_fixture_has_unpaired_pretooluse() {
    let path = fixtures_root()
        .join("session-with-failure")
        .join("hooks.jsonl");
    let mut pre = Vec::new();
    let mut post = Vec::new();
    for line in load_lines(&path) {
        let obj: Value = serde_json::from_str(&line).unwrap();
        let id = obj
            .get("tool_use_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match obj["hook_event_name"].as_str().unwrap() {
            "PreToolUse" => pre.extend(id),
            "PostToolUse" => post.extend(id),
            _ => {}
        }
    }
    let unpaired = pre.iter().filter(|id| !post.contains(id)).count();
    assert!(
        unpaired >= 1,
        "expected at least one PreToolUse without a matching PostToolUse (a failed tool call)"
    );
}
