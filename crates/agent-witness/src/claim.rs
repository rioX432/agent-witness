//! Claim-vs-reality evidence: the agent's final message beside the recorded
//! execution facts (ADR-0005, issue #63).
//!
//! This is the honesty-critical section of the report. It does **claim-aware
//! presentation, never claim extraction**: it shows the final assistant message
//! verbatim next to a filtered panel of recorded facts (test/build/lint commands
//! and their status, the count of unpaired calls, test files written/edited), plus
//! neutral routing cues. It **never** parses the prose into a claim, and **never**
//! adjudicates the claim's truth.
//!
//! The precision line (ADR-0005): the record may state what the message literally
//! contained and what the record literally contains; it must never say the claim
//! is false, contradicted, or that the agent lied, and it must never treat an
//! unpaired call (`no-result`) as a failure. A `no-result` is genuine ambiguity —
//! a failed Bash fires no completion hook (ADR-0001). Reducing and pointing at a
//! tension is in scope; the verdict is delegated to the reader (and, at scale, to
//! a future agent layer — never the CLI).

use agent_witness_core::{AgentEvent, Attribution, EventKind};
use serde::Serialize;
use serde_json::Value;

use crate::testcmd::classify_command;
use crate::timeline::{TimelineEntry, ToolStatus};

/// `tool_input` field naming a shell command (Bash).
const FIELD_COMMAND: &str = "command";
/// `tool_input` field naming the file a tool acted on.
const FIELD_FILE_PATH: &str = "file_path";
/// Top-level payload field wrapping a tool call's arguments.
const FIELD_TOOL_INPUT: &str = "tool_input";
/// `Stop`-event payload field carrying the agent's final message for the turn.
const FIELD_LAST_MESSAGE: &str = "last_assistant_message";

/// The claim-vs-reality panel: the final message and the recorded evidence beside
/// it. Present in a report only when there is something to contrast (a final
/// message, or test-like activity).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClaimVsReality {
    /// The agent's final message, verbatim — the last `Stop` event's
    /// `last_assistant_message`. `None` when no `Stop` carried one.
    pub final_message: Option<String>,
    /// Test/build/lint commands recorded, in call order, with observed status.
    pub test_commands: Vec<TestCommandRun>,
    /// Count of recorded tool calls with no paired result. Ambiguous by nature
    /// (a failed Bash emits no completion hook) — never reported as a failure.
    pub no_result_calls: usize,
    /// Test files written or edited, by path heuristic, in first-seen order.
    pub test_files: Vec<TestFile>,
    /// Neutral routing cues — restatements of the facts above that point at a
    /// tension without ever judging it. May be empty.
    pub cues: Vec<String>,
}

/// A recorded test-like command and its observed outcome.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TestCommandRun {
    /// The command line, flattened to one physical line.
    pub command: String,
    /// `test` / `build` / `lint` (see [`crate::testcmd`]).
    pub kind: &'static str,
    /// Observed status: `ok` / `failed` / `no-result` — the same labels the
    /// timeline uses; `no-result` is never called a failure.
    pub status: &'static str,
    /// Attribution of the call (ADR-0002).
    pub attribution: Attribution,
}

/// A file that looks like a test file (by path heuristic) and was written/edited.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TestFile {
    /// The file path, verbatim from `tool_input.file_path`.
    pub path: String,
    /// Tool names that referenced it (e.g. `Write`, `Edit`), in first-seen order.
    pub tools: Vec<String>,
    /// Attribution of the referencing call (ADR-0002).
    pub attribution: Attribution,
}

/// Build the claim-vs-reality panel, or `None` when there is nothing to contrast
/// (no final message and no test-like activity). Pure: a function of the recorded
/// events and their timeline.
pub fn build_claim_vs_reality(
    events: &[AgentEvent],
    entries: &[TimelineEntry],
) -> Option<ClaimVsReality> {
    let final_message = last_final_message(events);
    let test_commands = collect_test_commands(entries);
    let test_files = collect_test_files(entries);
    let no_result_calls = entries
        .iter()
        .filter(|e| e.tool_status == Some(ToolStatus::NoResult))
        .count();

    // Render a section only when there is a claim or test-like activity to place
    // it against; an unpaired-call count alone is not a contrast.
    if final_message.is_none() && test_commands.is_empty() && test_files.is_empty() {
        return None;
    }

    let cues = build_cues(final_message.as_deref(), &test_commands, no_result_calls);

    Some(ClaimVsReality {
        final_message,
        test_commands,
        no_result_calls,
        test_files,
        cues,
    })
}

/// The last `Stop` event's `last_assistant_message`, if any is a non-empty string.
/// `Stop` fires per turn, so the final one before session end is the session's
/// final word (ADR-0005).
fn last_final_message(events: &[AgentEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .filter(|e| e.kind == EventKind::Stop)
        .find_map(|e| {
            e.payload
                .get(FIELD_LAST_MESSAGE)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
        })
}

/// Test-like commands over the recorded Bash calls, in call order.
fn collect_test_commands(entries: &[TimelineEntry]) -> Vec<TestCommandRun> {
    entries
        .iter()
        .filter(|e| e.tool_status.is_some())
        .filter_map(|entry| {
            let command = tool_input_str(&entry.call, FIELD_COMMAND)?;
            let kind = classify_command(command)?;
            Some(TestCommandRun {
                command: one_line(command),
                kind: kind.label(),
                status: entry
                    .tool_status
                    .map(ToolStatus::label)
                    .unwrap_or("no-result"),
                attribution: entry.attribution,
            })
        })
        .collect()
}

/// Test files written/edited, deduped by path with the referencing tools kept
/// (so a read isn't assumed a write), in first-seen order.
fn collect_test_files(entries: &[TimelineEntry]) -> Vec<TestFile> {
    let mut files: Vec<TestFile> = Vec::new();
    for entry in entries.iter().filter(|e| e.tool_status.is_some()) {
        // Only writes/edits change a file; a bare Read is not a modification.
        // These are the tools that name their target via `file_path` (NotebookEdit
        // uses `notebook_path`, so it wouldn't resolve here anyway).
        if !matches!(entry.tag.as_str(), "Write" | "Edit" | "MultiEdit") {
            continue;
        }
        let Some(path) = tool_input_str(&entry.call, FIELD_FILE_PATH) else {
            continue;
        };
        if !is_test_file(path) {
            continue;
        }
        match files.iter_mut().find(|f| f.path == path) {
            Some(existing) => {
                if !existing.tools.contains(&entry.tag) {
                    existing.tools.push(entry.tag.clone());
                }
            }
            None => files.push(TestFile {
                path: path.to_string(),
                tools: vec![entry.tag.clone()],
                attribution: entry.attribution,
            }),
        }
    }
    files
}

/// Neutral routing cues — each one a restatement of recorded facts that points at
/// a tension without judging it. Deliberately conservative: when a signal is
/// absent or ambiguous, it emits nothing rather than risk an accusation.
fn build_cues(
    final_message: Option<&str>,
    test_commands: &[TestCommandRun],
    no_result_calls: usize,
) -> Vec<String> {
    let mut cues = Vec::new();

    let has_test = test_commands.iter().any(|c| c.kind == "test");
    let mentions_tests = final_message.is_some_and(mentions_tests);

    // The key discrepancy pointer: the message brings up tests, yet the record
    // holds no test command. Both halves are literal facts; no verdict is drawn.
    if mentions_tests && !has_test {
        cues.push(
            "The final message mentions tests, but no test-like command was \
             recorded in this session."
                .to_string(),
        );
    }

    // Recorded failures are a direct observation and safe to state as such.
    let failed_tests = test_commands
        .iter()
        .filter(|c| c.kind == "test" && c.status == "failed")
        .count();
    if failed_tests > 0 {
        cues.push(format!(
            "{failed_tests} test-like command(s) were recorded with a failed result."
        ));
    }

    // Unpaired calls are ambiguous — stated as not-observed, never as failure.
    if no_result_calls > 0 {
        cues.push(format!(
            "{no_result_calls} recorded tool call(s) have no result — a failed Bash \
             fires no completion hook, so an unpaired call is not observed as \
             success or failure."
        ));
    }

    cues
}

/// Whether the message's prose mentions tests. Word-anchored (a token whose
/// lowercase starts with `test`) so `latest`/`greatest`/`contest` do not match.
/// Best-effort by design: a miss simply omits the discrepancy cue — it never
/// produces a false one.
fn mentions_tests(message: &str) -> bool {
    message
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word.to_ascii_lowercase().starts_with("test"))
}

/// Heuristic: does this path look like a test file? Covers the common conventions
/// (a `tests`/`spec` directory, `test_*` / `*_test.*` / `*.test.*` / `*.spec.*` /
/// `*_spec.*` filenames). CamelCase `FooTest.java` and the like are a documented
/// gap — anchored patterns are used deliberately to keep false positives near zero
/// (`latest.rs` must not match).
fn is_test_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let segments: Vec<&str> = lower.split(['/', '\\']).collect();
    let in_test_dir = segments
        .iter()
        .any(|s| matches!(*s, "test" | "tests" | "__tests__" | "spec" | "specs"));
    let file = segments.last().copied().unwrap_or("");
    let name_signal = file.starts_with("test_")
        || file.contains("_test.")
        || file.contains(".test.")
        || file.contains(".spec.")
        || file.contains("_spec.");
    in_test_dir || name_signal
}

/// Read a string field from a tool call's `tool_input`.
fn tool_input_str<'a>(ev: &'a AgentEvent, key: &str) -> Option<&'a str> {
    ev.payload
        .get(FIELD_TOOL_INPUT)
        .and_then(|input| input.get(key))
        .and_then(Value::as_str)
}

/// Flatten a value to a single physical line: newlines, carriage returns, and
/// tabs become spaces so a command can never break a list row.
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_witness_core::{Source, CONFIDENCE_CERTAIN};
    use serde_json::json;

    use crate::timeline::build_timeline;

    fn ev(ts: i64, kind: EventKind, payload: Value) -> AgentEvent {
        AgentEvent::new(
            ts,
            "s",
            Source::Hooks,
            kind,
            Attribution::Direct,
            CONFIDENCE_CERTAIN,
            payload,
        )
    }

    fn call(ts: i64, tool: &str, id: &str, input: Value) -> AgentEvent {
        ev(
            ts,
            EventKind::ToolCall,
            json!({"tool_name": tool, "tool_use_id": id, "tool_input": input}),
        )
    }

    fn result(ts: i64, tool: &str, id: &str) -> AgentEvent {
        ev(
            ts,
            EventKind::ToolResult,
            json!({"tool_name": tool, "tool_use_id": id}),
        )
    }

    fn build(events: &[AgentEvent]) -> Option<ClaimVsReality> {
        let entries = build_timeline(events);
        build_claim_vs_reality(events, &entries)
    }

    #[test]
    fn none_when_no_message_and_no_test_activity() {
        let events = vec![
            call(1, "Bash", "t1", json!({"command": "ls"})),
            result(2, "Bash", "t1"),
        ];
        assert!(build(&events).is_none());
    }

    #[test]
    fn captures_verbatim_final_message_from_last_stop() {
        let events = vec![
            ev(
                1,
                EventKind::Stop,
                json!({"last_assistant_message": "first turn"}),
            ),
            call(2, "Bash", "t1", json!({"command": "cargo test"})),
            result(3, "Bash", "t1"),
            ev(
                4,
                EventKind::Stop,
                json!({"last_assistant_message": "done, tests pass"}),
            ),
        ];
        let cvr = build(&events).unwrap();
        // The LAST Stop wins, verbatim.
        assert_eq!(cvr.final_message.as_deref(), Some("done, tests pass"));
    }

    #[test]
    fn empty_or_absent_final_message_is_none() {
        let events = vec![
            call(1, "Bash", "t1", json!({"command": "cargo test"})),
            result(2, "Bash", "t1"),
            ev(3, EventKind::Stop, json!({"last_assistant_message": "   "})),
            ev(4, EventKind::Stop, json!({"stop_hook_active": false})),
        ];
        let cvr = build(&events).unwrap();
        assert_eq!(cvr.final_message, None);
        // But the test command still surfaced.
        assert_eq!(cvr.test_commands.len(), 1);
    }

    #[test]
    fn test_command_records_kind_and_status() {
        let events = vec![
            call(
                1,
                "Bash",
                "t1",
                json!({"command": "cargo test --workspace"}),
            ),
            result(2, "Bash", "t1"),
            call(3, "Bash", "t2", json!({"command": "cargo build"})),
            result(4, "Bash", "t2"),
        ];
        let cvr = build(&events).unwrap();
        assert_eq!(cvr.test_commands.len(), 2);
        assert_eq!(cvr.test_commands[0].kind, "test");
        assert_eq!(cvr.test_commands[0].status, "ok");
        assert_eq!(cvr.test_commands[1].kind, "build");
    }

    #[test]
    fn unpaired_test_command_is_no_result_not_failed() {
        // A cargo test with no PostToolUse (the failed-Bash shape).
        let events = vec![call(1, "Bash", "t1", json!({"command": "cargo test"}))];
        let cvr = build(&events).unwrap();
        assert_eq!(cvr.test_commands[0].status, "no-result");
        assert_eq!(cvr.no_result_calls, 1);
    }

    #[test]
    fn test_files_detected_by_heuristic_reads_excluded() {
        let events = vec![
            call(1, "Write", "t1", json!({"file_path": "/p/tests/foo.rs"})),
            result(2, "Write", "t1"),
            call(3, "Edit", "t2", json!({"file_path": "/p/src/app.test.ts"})),
            result(4, "Edit", "t2"),
            // A plain source file must not be treated as a test file.
            call(5, "Write", "t3", json!({"file_path": "/p/src/latest.rs"})),
            result(6, "Write", "t3"),
            // A Read of a test file is not a write.
            call(7, "Read", "t4", json!({"file_path": "/p/tests/bar.rs"})),
            result(8, "Read", "t4"),
        ];
        let cvr = build(&events).unwrap();
        let paths: Vec<&str> = cvr.test_files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["/p/tests/foo.rs", "/p/src/app.test.ts"]);
    }

    #[test]
    fn cue_points_at_tension_without_a_verdict() {
        // Final message mentions tests, but no test command was recorded.
        let events = vec![ev(
            1,
            EventKind::Stop,
            json!({"last_assistant_message": "All done — tests pass and the build is green."}),
        )];
        let cvr = build(&events).unwrap();
        assert!(cvr
            .cues
            .iter()
            .any(|c| c.contains("mentions tests") && c.contains("no test-like command")));
    }

    #[test]
    fn no_test_mention_no_discrepancy_cue() {
        let events = vec![ev(
            1,
            EventKind::Stop,
            json!({"last_assistant_message": "Refactored the parser and updated docs."}),
        )];
        let cvr = build(&events).unwrap();
        assert!(cvr.cues.iter().all(|c| !c.contains("mentions tests")));
    }

    #[test]
    fn latest_in_prose_does_not_count_as_a_test_mention() {
        // `latest` contains the substring "test" but must not trip the cue.
        let events = vec![
            ev(
                1,
                EventKind::Stop,
                json!({"last_assistant_message": "Pulled the latest changes."}),
            ),
            call(2, "Write", "t1", json!({"file_path": "/p/tests/x.rs"})),
            result(3, "Write", "t1"),
        ];
        let cvr = build(&events).unwrap();
        assert!(cvr.cues.iter().all(|c| !c.contains("mentions tests")));
    }

    /// The precision policy, enforced: no cue may ever contain an adjudicating
    /// word. This is the one unforgivable-bug guard (ADR-0002 / ADR-0005).
    #[test]
    fn cues_never_contain_a_verdict_word() {
        let events = vec![
            ev(
                1,
                EventKind::Stop,
                json!({"last_assistant_message": "tests pass, all green"}),
            ),
            call(2, "Bash", "t1", json!({"command": "cargo test"})), // no result → no-result
            call(3, "Bash", "t2", json!({"command": "cargo build"})),
            ev(
                4,
                EventKind::ToolFailure,
                json!({"tool_name": "Bash", "tool_use_id": "t3"}),
            ),
        ];
        let cvr = build(&events).unwrap();
        let forbidden = [
            "lied",
            "false",
            "contradict",
            "did not pass",
            "the agent",
            "dishonest",
        ];
        for cue in &cvr.cues {
            let lower = cue.to_ascii_lowercase();
            for word in forbidden {
                assert!(!lower.contains(word), "cue must not adjudicate: {cue:?}");
            }
        }
    }
}
