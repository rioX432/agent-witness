//! Golden/snapshot tests for `agent-witness report` (issue #6).
//!
//! Determinism is the verification point (docs/test-strategy.md): the report is
//! a pure function of the recorded event data, so a fixture normalized with
//! fixed timestamps renders byte-for-byte stable markdown. Golden files live
//! under `tests/golden/`. To regenerate them after an intentional change:
//!
//! ```text
//! UPDATE_GOLDEN=1 cargo nextest run -p agent-witness report_golden
//! ```
//!
//! The disclaimer-presence test is a mechanical honesty guard (ADR-0002): every
//! report MUST carry the observation-scope disclaimer, in both formats.

use std::path::{Path, PathBuf};

use agent_witness::report::{
    build_report, JsonExporter, MarkdownExporter, SessionExporter, OBSERVATION_SCOPE_MARKER,
};
use agent_witness_core::{normalize, AgentEvent, SessionRead};

/// Base timestamp for normalized fixtures (2023-11-14 22:13:20Z). Events are
/// spaced a fixed step apart so relative offsets are stable and readable.
const BASE_TS: i64 = 1_700_000_000_000;
const TS_STEP: i64 = 1_000;

/// Normalize a fixture's `hooks.jsonl` into events with deterministic times.
fn fixture_events(scenario: &str) -> Vec<AgentEvent> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join(scenario)
        .join("hooks.jsonl");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            let value = serde_json::from_str(line).expect("fixture line is valid json");
            let ts = BASE_TS + (i as i64) * TS_STEP;
            normalize(&value, ts, &format!("raw-{i}")).expect("fixture line normalizes")
        })
        .collect()
}

fn read(events: Vec<AgentEvent>, skipped: usize) -> SessionRead {
    SessionRead {
        events,
        skipped_lines: skipped,
    }
}

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(format!("{name}.md"))
}

/// Compare `actual` against the golden file, or (re)write it under UPDATE_GOLDEN.
fn assert_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).expect("create golden dir");
        std::fs::write(&path, actual).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden {}: {e}\nRun with UPDATE_GOLDEN=1 to create it",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "golden mismatch for {name}; run UPDATE_GOLDEN=1 to update if intended"
    );
}

/// Render a scenario's markdown report deterministically (start = first event).
fn render_markdown(scenario: &str) -> String {
    let read = read(fixture_events(scenario), 0);
    let report = build_report(scenario, &read, Some(BASE_TS));
    MarkdownExporter.export(&report).expect("markdown export")
}

#[test]
fn golden_basic_report() {
    assert_golden("report_basic", &render_markdown("session-basic"));
}

#[test]
fn golden_failure_report() {
    let markdown = render_markdown("session-with-failure");
    // The unpaired (failed) Bash call must be surfaced as no-result, not success.
    assert!(
        markdown.contains("no-result"),
        "failure report must surface the unpaired call:\n{markdown}"
    );
    assert_golden("report_failure", &markdown);
}

#[test]
fn golden_flagged_report() {
    // A session containing a destructive command must render the flags section,
    // severity-sorted (critical `rm -rf ~/` above warning `git push --force`).
    let markdown = render_markdown("session-flagged");
    assert!(
        markdown.contains("## Flagged commands"),
        "flagged report must render the flags section:\n{markdown}"
    );
    // Honesty framing: class-only, never intent (Core Value 1 / ADR-0002).
    assert!(
        markdown.contains("no claim about intent"),
        "flags section must carry the class-not-intent caveat:\n{markdown}"
    );
    // Critical row appears before the warning row.
    let critical = markdown
        .find("recursive force-remove targeting a home or root path")
        .expect("critical rationale present");
    let warning = markdown
        .find("force-push rewrites remote history")
        .expect("warning rationale present");
    assert!(
        critical < warning,
        "critical must sort before warning:\n{markdown}"
    );
    assert_golden("report_flagged", &markdown);
}

#[test]
fn clean_session_has_no_flagged_section() {
    // The happy-path session runs only `rustc --version` — nothing destructive.
    let markdown = render_markdown("session-basic");
    assert!(
        !markdown.contains("## Flagged commands"),
        "a clean session must omit the flags section:\n{markdown}"
    );
}

#[test]
fn disclaimer_present_in_both_formats() {
    // Mechanical honesty guard (ADR-0002): the observation-scope disclaimer must
    // appear in every report, markdown and JSON alike.
    let session_read = read(fixture_events("session-basic"), 0);
    let report = build_report("session-basic", &session_read, Some(BASE_TS));

    let markdown = MarkdownExporter.export(&report).expect("markdown export");
    let json = JsonExporter.export(&report).expect("json export");

    assert!(markdown.contains(OBSERVATION_SCOPE_MARKER));
    assert!(markdown.contains("## Observation scope"));
    assert!(markdown.contains("bash script.sh"));
    assert!(json.contains(OBSERVATION_SCOPE_MARKER));
}

#[test]
fn json_output_has_expected_shape() {
    let session_read = read(fixture_events("session-basic"), 0);
    let report = build_report("session-basic", &session_read, Some(BASE_TS));
    let json = JsonExporter.export(&report).expect("json export");
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");

    assert_eq!(value["session_id"], "session-basic");
    // session-basic writes one file and runs one command.
    assert_eq!(value["summary"]["touched_files"], 1);
    assert_eq!(value["summary"]["commands"], 1);
    assert_eq!(value["summary"]["tool_calls"], 2);
    assert_eq!(
        value["touched_files"][0]["path"],
        "/home/user/project/hello.rs"
    );
    assert_eq!(value["touched_files"][0]["attribution"], "direct");
    assert_eq!(value["commands"][0]["command"], "rustc --version");
    assert_eq!(value["commands"][0]["status"], "ok");
    assert!(value["timeline"].is_array());
    assert!(value["disclaimer"].is_array());
}

#[test]
fn json_output_reports_the_failure_as_no_result() {
    let session_read = read(fixture_events("session-with-failure"), 0);
    let report = build_report("session-with-failure", &session_read, Some(BASE_TS));
    let json = JsonExporter.export(&report).expect("json export");
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");

    // The first Bash call (`cat missing-config.toml`) has no PostToolUse hook.
    assert_eq!(value["commands"][0]["command"], "cat missing-config.toml");
    assert_eq!(value["commands"][0]["status"], "no-result");
    assert_eq!(value["commands"][1]["command"], "ls -la");
    assert_eq!(value["commands"][1]["status"], "ok");
}

#[test]
fn json_output_carries_flags_severity_sorted() {
    let session_read = read(fixture_events("session-flagged"), 0);
    let report = build_report("session-flagged", &session_read, Some(BASE_TS));
    let json = JsonExporter.export(&report).expect("json export");
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");

    let flags = value["flags"].as_array().expect("flags is an array");
    assert_eq!(flags.len(), 2);
    // Critical (`rm -rf ~/`) sorts before the warning (force-push) despite the
    // warning command running first; severity serializes snake_case.
    assert_eq!(value["flags"][0]["severity"], "critical");
    assert_eq!(value["flags"][0]["pattern"], "rm-rf-home-root");
    assert_eq!(value["flags"][0]["command"], "rm -rf ~/");
    assert_eq!(value["flags"][1]["severity"], "warning");
    assert_eq!(value["flags"][1]["pattern"], "git-force-push");
}
