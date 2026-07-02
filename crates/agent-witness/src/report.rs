//! `agent-witness report <session>`: a shareable markdown (or `--json`) audit of
//! one recorded session — Core Value 3 (evidence you can paste into a PR/issue).
//!
//! The report is a **pure function of the recorded event data** (plus the
//! session's start time): it reads no wall-clock, so the same session always
//! renders the same bytes — the property the golden tests depend on. All times
//! shown derive from the `ts` carried on each [`AgentEvent`].
//!
//! Honesty (ADR-0002): the report never claims more than was observed. Touched
//! files and commands carry their `attribution`; a failed Bash call surfaces as
//! `no-result`, never as a success (see [`crate::timeline`]); the corrupt-line
//! count is reported; and every report ends with an observation-scope
//! disclaimer stating the hooks-based limits (e.g. `bash script.sh` internals
//! are invisible). The disclaimer is mandatory — see [`OBSERVATION_SCOPE_MARKER`].
//!
//! Formats are pluggable via [`SessionExporter`] so future interop targets
//! (e.g. cursor/agent-trace, v0.2) slot in without touching the report builder.
//! v0.1 ships exactly two exporters: [`MarkdownExporter`] and [`JsonExporter`].

use agent_witness_core::{AgentEvent, Attribution, SessionRead, Source};
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::timefmt::{format_duration_ms, format_offset_ms, format_utc};
use crate::timeline::{build_timeline, tool_call_count, TimelineEntry};

/// Stable substring guaranteed to appear in every report's observation-scope
/// disclaimer (markdown body and JSON `disclaimer` array alike). Tests assert on
/// this so the honesty guarantee (ADR-0002) can never regress silently.
pub const OBSERVATION_SCOPE_MARKER: &str = "records only what Claude Code hooks report";

/// `tool_input` field naming the file a tool acted on.
const FIELD_FILE_PATH: &str = "file_path";
/// `tool_input` field carrying a shell command (Bash).
const FIELD_COMMAND: &str = "command";
/// Top-level payload field wrapping a tool call's arguments.
const FIELD_TOOL_INPUT: &str = "tool_input";
/// Placeholder for an absent scalar value.
const ABSENT: &str = "-";

/// A fully-built, format-independent session report. Serializes directly as the
/// `--json` output, and is rendered to markdown by [`MarkdownExporter`]; both
/// formats therefore carry exactly the same data.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionReport {
    /// Session id this report describes.
    pub session_id: String,
    /// Headline counts and timing.
    pub summary: Summary,
    /// Distinct files named by tool `file_path` inputs, in first-seen order.
    pub touched_files: Vec<TouchedFile>,
    /// Shell commands observed via Bash `tool_input.command`, in order.
    pub commands: Vec<CommandRun>,
    /// One condensed row per timeline entry (calls paired with their results).
    pub timeline: Vec<TimelineDigestEntry>,
    /// Observation-scope disclaimer lines (always present; ADR-0002).
    pub disclaimer: Vec<String>,
}

/// Headline metrics for the summary section.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    /// Session start time, Unix epoch milliseconds (meta start, else first event).
    pub started_ms: Option<i64>,
    /// Wall span from the first to the last event, milliseconds.
    pub duration_ms: Option<i64>,
    /// Number of tool invocations (`ToolCall` events).
    pub tool_calls: usize,
    /// Number of distinct touched files.
    pub touched_files: usize,
    /// Number of executed commands (Bash calls).
    pub commands: usize,
    /// Total successfully-parsed events.
    pub events: usize,
    /// Corrupted/unreadable lines skipped while reading (honesty surface).
    pub corrupt_lines: usize,
}

/// A file named by one or more tool `file_path` inputs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TouchedFile {
    /// The file path, verbatim from `tool_input.file_path`.
    pub path: String,
    /// Tool names that referenced this path, in first-seen order (e.g. `Write`,
    /// `Edit`, `Read`). Kept so a reader can tell a write from a mere read
    /// rather than assuming every referenced file was modified.
    pub tools: Vec<String>,
    /// Attribution of the referencing call (ADR-0002).
    pub attribution: Attribution,
}

/// A shell command observed via a Bash tool call.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandRun {
    /// The command line, flattened to a single physical line for display.
    pub command: String,
    /// Observed outcome: `ok`, `failed`, or `no-result` (an unpaired call — e.g.
    /// a failed Bash, which fires no completion hook — is never called a success).
    pub status: &'static str,
    /// Elapsed time if the completion hook reported one.
    pub duration_ms: Option<i64>,
    /// Attribution of the call (ADR-0002).
    pub attribution: Attribution,
}

/// One condensed timeline row (a call folded together with its result).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineDigestEntry {
    /// Offset from the first event, milliseconds.
    pub offset_ms: i64,
    /// Attribution of the primary event (ADR-0002).
    pub attribution: Attribution,
    /// Origin of the primary event.
    pub source: Source,
    /// Short tag (tool name, or `SESSION`/`PROMPT`/`STOP`/`ERROR`).
    pub tag: String,
    /// Tool status label for tool rows; `None` for meta rows.
    pub status: Option<&'static str>,
    /// Elapsed time for tool rows whose result reported one.
    pub duration_ms: Option<i64>,
    /// One-line human summary (file path, command, prompt text, …).
    pub summary: String,
}

/// Build a [`SessionReport`] from a session read.
///
/// `started_ms` is the session's recorded start (e.g. `meta.created_ts`); when
/// `None`, the first event's time is used. Pure: no I/O, no wall-clock.
pub fn build_report(
    session_id: &str,
    read: &SessionRead,
    started_ms: Option<i64>,
) -> SessionReport {
    let entries = build_timeline(&read.events);
    let first_ts = read.events.first().map(|e| e.ts);
    let base_ts = first_ts.unwrap_or(0);
    let duration_ms = match (read.events.first(), read.events.last()) {
        (Some(first), Some(last)) => Some(last.ts - first.ts),
        _ => None,
    };

    let touched_files = collect_touched_files(&entries);
    let commands = collect_commands(&entries);
    let timeline = collect_timeline(&entries, base_ts);

    let summary = Summary {
        started_ms: started_ms.or(first_ts),
        duration_ms,
        // Count invocations (`ToolCall` events), the same definition `ls`/TUI
        // use — never orphan results, which `build_timeline` also surfaces.
        tool_calls: tool_call_count(&read.events),
        touched_files: touched_files.len(),
        commands: commands.len(),
        events: read.events.len(),
        corrupt_lines: read.skipped_lines,
    };

    SessionReport {
        session_id: session_id.to_string(),
        summary,
        touched_files,
        commands,
        timeline,
        disclaimer: disclaimer_lines(read.skipped_lines),
    }
}

/// Distinct files referenced by tool `file_path` inputs, in first-seen order.
fn collect_touched_files(entries: &[TimelineEntry]) -> Vec<TouchedFile> {
    let mut files: Vec<TouchedFile> = Vec::new();
    for entry in entries.iter().filter(|e| e.tool_status.is_some()) {
        let Some(path) = tool_input_str(&entry.call, FIELD_FILE_PATH) else {
            continue;
        };
        match files.iter_mut().find(|f| f.path == path) {
            Some(existing) => {
                if !existing.tools.contains(&entry.tag) {
                    existing.tools.push(entry.tag.clone());
                }
            }
            None => files.push(TouchedFile {
                path: path.to_string(),
                tools: vec![entry.tag.clone()],
                attribution: entry.attribution,
            }),
        }
    }
    files
}

/// Commands observed via Bash `tool_input.command`, in call order.
fn collect_commands(entries: &[TimelineEntry]) -> Vec<CommandRun> {
    entries
        .iter()
        .filter(|e| e.tool_status.is_some())
        .filter_map(|entry| {
            let command = tool_input_str(&entry.call, FIELD_COMMAND)?;
            Some(CommandRun {
                command: one_line(command),
                status: entry.tool_status.map(|s| s.label()).unwrap_or(ABSENT),
                duration_ms: entry.duration_ms,
                attribution: entry.attribution,
            })
        })
        .collect()
}

/// One condensed digest row per timeline entry.
fn collect_timeline(entries: &[TimelineEntry], base_ts: i64) -> Vec<TimelineDigestEntry> {
    entries
        .iter()
        .map(|entry| TimelineDigestEntry {
            offset_ms: entry.ts - base_ts,
            attribution: entry.attribution,
            source: entry.source,
            tag: entry.tag.clone(),
            status: entry.tool_status.map(|s| s.label()),
            duration_ms: entry.duration_ms,
            summary: entry.summary.clone(),
        })
        .collect()
}

/// Read a string field from a tool call's `tool_input`.
fn tool_input_str<'a>(ev: &'a AgentEvent, key: &str) -> Option<&'a str> {
    ev.payload
        .get(FIELD_TOOL_INPUT)
        .and_then(|input| input.get(key))
        .and_then(Value::as_str)
}

/// The observation-scope disclaimer, one sentence per line. Always emitted so a
/// report can never overclaim (ADR-0002); the trailing line reports how many
/// corrupt lines were skipped.
fn disclaimer_lines(corrupt_lines: usize) -> Vec<String> {
    vec![
        format!(
            "agent-witness {OBSERVATION_SCOPE_MARKER}: tool calls and their \
             inputs, not their side effects."
        ),
        "A command run via Bash is recorded by its command line only; what it \
         does internally (e.g. `bash script.sh`) is not observed."
            .to_string(),
        "A failed Bash call fires no completion hook, so it appears as a call \
         with no result — never as a success."
            .to_string(),
        format!(
            "{corrupt_lines} corrupt/unreadable line(s) were skipped while reading this session."
        ),
    ]
}

/// Flatten a value to a single physical line so it cannot break a list row:
/// newlines, carriage returns, and tabs become spaces.
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect()
}

/// Full attribution word for report prose.
fn attribution_word(attribution: Attribution) -> &'static str {
    match attribution {
        Attribution::Direct => "direct",
        Attribution::Observed => "observed",
        Attribution::Inferred => "inferred",
    }
}

/// Renders a [`SessionReport`] into a concrete output format. New interop
/// formats (v0.2) implement this trait; the report builder stays untouched.
pub trait SessionExporter {
    /// Render `report` to a self-contained string (markdown, JSON, …).
    fn export(&self, report: &SessionReport) -> Result<String>;
}

/// Renders a report as a shareable markdown document (the default output).
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkdownExporter;

/// Renders a report as pretty-printed JSON (`--json`) — the same data as the
/// markdown, in machine-readable form.
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonExporter;

impl SessionExporter for MarkdownExporter {
    fn export(&self, report: &SessionReport) -> Result<String> {
        Ok(render_markdown(report))
    }
}

impl SessionExporter for JsonExporter {
    fn export(&self, report: &SessionReport) -> Result<String> {
        let mut out = serde_json::to_string_pretty(report)?;
        out.push('\n');
        Ok(out)
    }
}

/// Render the markdown report body. Deterministic function of `report`.
fn render_markdown(report: &SessionReport) -> String {
    let mut out = String::new();

    push_line(
        &mut out,
        &format!("# Session report: {}", report.session_id),
    );
    push_line(&mut out, "");
    push_line(
        &mut out,
        "_Observation only — records what Claude Code hooks reported. See the scope note at the end._",
    );
    push_line(&mut out, "");

    render_summary(&mut out, &report.summary);
    render_touched_files(&mut out, &report.touched_files);
    render_commands(&mut out, &report.commands);
    render_timeline(&mut out, &report.timeline);
    render_scope(&mut out, &report.disclaimer);

    out
}

fn render_summary(out: &mut String, summary: &Summary) {
    push_line(out, "## Summary");
    push_line(out, "");
    let started = summary
        .started_ms
        .map(format_utc)
        .unwrap_or_else(|| ABSENT.to_string());
    let duration = summary
        .duration_ms
        .map(format_duration_ms)
        .unwrap_or_else(|| ABSENT.to_string());
    push_line(out, &format!("- Started: {started}"));
    push_line(out, &format!("- Duration: {duration}"));
    push_line(out, &format!("- Tool calls: {}", summary.tool_calls));
    push_line(out, &format!("- Touched files: {}", summary.touched_files));
    push_line(out, &format!("- Commands: {}", summary.commands));
    push_line(out, &format!("- Events: {}", summary.events));
    push_line(out, &format!("- Corrupt lines: {}", summary.corrupt_lines));
    push_line(out, "");
}

fn render_touched_files(out: &mut String, files: &[TouchedFile]) {
    push_line(out, "## Touched files");
    push_line(out, "");
    push_line(out, "_From `tool_input.file_path`; attribution noted._");
    push_line(out, "");
    if files.is_empty() {
        push_line(out, "_None observed._");
        push_line(out, "");
        return;
    }
    for file in files {
        push_line(
            out,
            &format!(
                "- `{}` — {} ({})",
                file.path,
                file.tools.join(", "),
                attribution_word(file.attribution),
            ),
        );
    }
    push_line(out, "");
}

fn render_commands(out: &mut String, commands: &[CommandRun]) {
    push_line(out, "## Executed commands");
    push_line(out, "");
    push_line(out, "_From Bash `tool_input.command`._");
    push_line(out, "");
    if commands.is_empty() {
        push_line(out, "_None observed._");
        push_line(out, "");
        return;
    }
    for cmd in commands {
        let duration = cmd
            .duration_ms
            .map(|d| format!(", {}", format_duration_ms(d)))
            .unwrap_or_default();
        push_line(
            out,
            &format!(
                "- `{}` — {}{} ({})",
                cmd.command,
                cmd.status,
                duration,
                attribution_word(cmd.attribution),
            ),
        );
    }
    push_line(out, "");
}

fn render_timeline(out: &mut String, timeline: &[TimelineDigestEntry]) {
    push_line(out, "## Timeline");
    push_line(out, "");
    if timeline.is_empty() {
        push_line(out, "_No events recorded._");
        push_line(out, "");
        return;
    }
    for entry in timeline {
        let offset = format_offset_ms(entry.offset_ms);
        let mut tag = entry.tag.clone();
        if let Some(status) = entry.status {
            tag.push_str(&format!(" [{status}]"));
            if let Some(duration) = entry.duration_ms {
                tag.push_str(&format!(" ({})", format_duration_ms(duration)));
            }
        }
        let summary = if entry.summary.is_empty() {
            String::new()
        } else {
            format!(" · {}", entry.summary)
        };
        push_line(
            out,
            &format!(
                "- `{offset}` · {tag}{summary} · {}",
                attribution_word(entry.attribution),
            ),
        );
    }
    push_line(out, "");
}

fn render_scope(out: &mut String, disclaimer: &[String]) {
    push_line(out, "## Observation scope");
    push_line(out, "");
    for line in disclaimer {
        push_line(out, line);
    }
}

/// Append one line and a trailing newline.
fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_witness_core::{EventKind, CONFIDENCE_CERTAIN};
    use serde_json::json;

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

    fn read_with(events: Vec<AgentEvent>, skipped: usize) -> SessionRead {
        SessionRead {
            events,
            skipped_lines: skipped,
        }
    }

    #[test]
    fn summary_counts_files_commands_and_duration() {
        let events = vec![
            ev(1_000, EventKind::SessionStart, json!({"source": "startup"})),
            ev(
                2_000,
                EventKind::ToolCall,
                json!({"tool_name": "Write", "tool_use_id": "t1",
                       "tool_input": {"file_path": "/p/a.rs"}}),
            ),
            ev(
                3_000,
                EventKind::ToolResult,
                json!({"tool_name": "Write", "tool_use_id": "t1", "duration_ms": 5}),
            ),
            ev(
                4_000,
                EventKind::ToolCall,
                json!({"tool_name": "Bash", "tool_use_id": "t2",
                       "tool_input": {"command": "ls"}}),
            ),
        ];
        let report = build_report("s", &read_with(events, 2), None);
        assert_eq!(report.summary.tool_calls, 2);
        assert_eq!(report.summary.touched_files, 1);
        assert_eq!(report.summary.commands, 1);
        assert_eq!(report.summary.events, 4);
        assert_eq!(report.summary.corrupt_lines, 2);
        assert_eq!(report.summary.duration_ms, Some(3_000));
        assert_eq!(report.summary.started_ms, Some(1_000));
    }

    #[test]
    fn touched_file_records_tools_and_dedups_by_path() {
        let events = vec![
            ev(
                1,
                EventKind::ToolCall,
                json!({"tool_name": "Write", "tool_use_id": "t1",
                       "tool_input": {"file_path": "/p/a.rs"}}),
            ),
            ev(
                2,
                EventKind::ToolResult,
                json!({"tool_name": "Write", "tool_use_id": "t1"}),
            ),
            ev(
                3,
                EventKind::ToolCall,
                json!({"tool_name": "Edit", "tool_use_id": "t2",
                       "tool_input": {"file_path": "/p/a.rs"}}),
            ),
        ];
        let report = build_report("s", &read_with(events, 0), None);
        assert_eq!(report.touched_files.len(), 1);
        assert_eq!(report.touched_files[0].path, "/p/a.rs");
        assert_eq!(report.touched_files[0].tools, vec!["Write", "Edit"]);
    }

    #[test]
    fn failed_bash_command_is_no_result_not_success() {
        // A Bash call with no matching PostToolUse (the empirical failure shape).
        let events = vec![ev(
            1,
            EventKind::ToolCall,
            json!({"tool_name": "Bash", "tool_use_id": "t1",
                   "tool_input": {"command": "cat missing"}}),
        )];
        let report = build_report("s", &read_with(events, 0), None);
        assert_eq!(report.commands.len(), 1);
        assert_eq!(report.commands[0].status, "no-result");
        assert_eq!(report.commands[0].command, "cat missing");
    }

    #[test]
    fn multiline_command_is_flattened_to_one_line() {
        let events = vec![ev(
            1,
            EventKind::ToolCall,
            json!({"tool_name": "Bash", "tool_use_id": "t1",
                   "tool_input": {"command": "echo a\necho b"}}),
        )];
        let report = build_report("s", &read_with(events, 0), None);
        assert_eq!(report.commands[0].command, "echo a echo b");
    }

    #[test]
    fn disclaimer_is_always_present_and_reports_corrupt_count() {
        let report = build_report("s", &read_with(vec![], 3), None);
        assert!(report
            .disclaimer
            .iter()
            .any(|l| l.contains(OBSERVATION_SCOPE_MARKER)));
        assert!(report.disclaimer.iter().any(|l| l.contains("3 corrupt")));
    }

    #[test]
    fn markdown_and_json_both_carry_the_disclaimer() {
        let report = build_report("s", &read_with(vec![], 0), None);
        let md = MarkdownExporter.export(&report).unwrap();
        let js = JsonExporter.export(&report).unwrap();
        assert!(md.contains(OBSERVATION_SCOPE_MARKER));
        assert!(md.contains("## Observation scope"));
        assert!(js.contains(OBSERVATION_SCOPE_MARKER));
    }

    #[test]
    fn json_round_trips_to_the_same_report() {
        let events = vec![ev(
            1,
            EventKind::ToolCall,
            json!({"tool_name": "Bash", "tool_use_id": "t1",
                   "tool_input": {"command": "ls"}}),
        )];
        let report = build_report("s", &read_with(events, 0), None);
        let js = JsonExporter.export(&report).unwrap();
        let parsed: Value = serde_json::from_str(&js).unwrap();
        assert_eq!(parsed["session_id"], json!("s"));
        assert_eq!(parsed["summary"]["commands"], json!(1));
        assert_eq!(parsed["commands"][0]["status"], json!("no-result"));
        assert!(parsed["disclaimer"].is_array());
    }

    #[test]
    fn empty_session_renders_placeholders_without_panic() {
        let report = build_report("empty", &read_with(vec![], 0), None);
        let md = MarkdownExporter.export(&report).unwrap();
        assert!(md.contains("_None observed._"));
        assert!(md.contains("_No events recorded._"));
        assert_eq!(report.summary.duration_ms, None);
    }
}
