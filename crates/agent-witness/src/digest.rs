//! `agent-witness digest`: a cross-session **delegation ledger** (issue #45).
//!
//! Aggregates every recorded session whose start falls within a time window into
//! a FACTUAL, per-project summary: session counts and durations, user prompts,
//! tool calls, files touched, commands run, destructive-class command-flag
//! counts, and per-model token totals. Markdown or `--json`.
//!
//! Honesty is the whole point of the framing (Core Value 1 / ADR-0002):
//!
//! - **Facts only.** The digest reports counts, durations, and token totals. It
//!   makes NO judgment about efficiency, model choice, or command safety — those
//!   belong to the agent/audit layer (a NON-GOALS boundary), never the CLI. The
//!   output never uses judgment words; command flag counts are pattern-matcher
//!   hits, not verdicts.
//! - **Usage unavailable is not zero.** A session with no `usage.json` sidecar is
//!   counted as "usage unavailable" and contributes ZERO to token totals — a
//!   distinct state from a session that genuinely used zero tokens.
//! - **User prompts only.** The prompt count includes only `Prompt` events from
//!   [`Source::Hooks`]; the transcript adapter also emits `Prompt` events
//!   ([`Source::Transcript`], assistant prose) which are NOT user prompts and are
//!   excluded.
//! - **First-cwd grouping, disclosed.** A session is attributed to the FIRST cwd
//!   observed in its events; a session that spans several directories is counted
//!   in `multi_cwd_sessions` so the attribution is never silent. A session with
//!   no observed cwd lands in the `"unknown project"` bucket.
//!
//! [`build_digest`] is a PURE function of already-read per-session inputs plus an
//! injected `now_ms`/`since_ms`, so window boundaries and aggregation are
//! deterministic and golden-testable. Window boundaries are UTC calendar days
//! (matching the repo's UTC-only time display), computed by [`window_since_ms`].
//! The store I/O ([`collect_session_inputs`]) is a thin, separately testable seam.

use std::collections::{BTreeMap, BTreeSet};

use agent_witness_core::{
    AgentEvent, EventKind, ModelUsage, SessionStore, SessionUsage, Source, StoreError,
};
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;

use crate::claim::test_command_runs;
use crate::flags::{flags_for_command, FlagSeverity};
use crate::inventory::parse_relative_ms;
use crate::timefmt::{format_duration_ms, format_utc};

/// Milliseconds per second.
const MS_PER_SEC: i64 = 1_000;
/// Seconds per day (a UTC calendar day; the Unix epoch is a UTC day boundary, so
/// day arithmetic on epoch seconds is exact).
const SECS_PER_DAY: i64 = 86_400;
/// Days to look back for `--week`: the last 7 UTC calendar days *including today*
/// means the lower bound is midnight of the day 6 days before today.
const WEEK_LOOKBACK_DAYS: i64 = 6;

/// Display name and group key for sessions with no observed cwd. Kept distinct
/// from any real path via a dedicated [`ProjectKey::Unknown`] map key, so a real
/// directory literally named this never merges into the bucket.
const UNKNOWN_PROJECT: &str = "unknown project";

/// Payload field carrying the working directory of a hook event.
const FIELD_CWD: &str = "cwd";
/// Tool-call payload field naming the invoked tool.
const FIELD_TOOL_NAME: &str = "tool_name";
/// Top-level payload field wrapping a tool call's arguments.
const FIELD_TOOL_INPUT: &str = "tool_input";
/// `tool_input` field naming the file a tool acted on.
const FIELD_FILE_PATH: &str = "file_path";
/// `tool_input` field carrying a shell command (Bash).
const FIELD_COMMAND: &str = "command";
/// Tool name whose calls are counted as executed commands.
const BASH_TOOL: &str = "Bash";

/// Which time-window selector produced this digest. Serialized `snake_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowKind {
    /// The current UTC calendar day.
    Today,
    /// The last 7 UTC calendar days, including today.
    Week,
    /// A relative look-back (`--since <dur>`).
    Since,
    /// No lower bound — all recorded history.
    All,
}

/// The requested time window. Constructed at the CLI edge (mutual-exclusivity and
/// duration parsing happen in [`window_mode_from_flags`]); [`window_since_ms`]
/// turns it into a concrete lower bound given an injected `now_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowMode {
    /// Current UTC calendar day.
    Today,
    /// Last 7 UTC calendar days including today.
    Week,
    /// Relative look-back of `span_ms` before now.
    Since {
        /// Parsed relative span in milliseconds.
        span_ms: i64,
    },
    /// All recorded history (no lower bound).
    All,
}

impl WindowMode {
    /// The serializable discriminator for this mode.
    fn kind(self) -> WindowKind {
        match self {
            WindowMode::Today => WindowKind::Today,
            WindowMode::Week => WindowKind::Week,
            WindowMode::Since { .. } => WindowKind::Since,
            WindowMode::All => WindowKind::All,
        }
    }
}

/// Already-read inputs for one session, handed to the pure aggregator.
///
/// Produced by [`collect_session_inputs`] so [`build_digest`] does no I/O and can
/// be exercised with fixtures. `usage` is `None` when the session has no
/// `usage.json` sidecar (or it was unreadable) — the "usage unavailable" state,
/// which is NOT the same as zero tokens.
#[derive(Debug, Clone)]
pub struct SessionInput {
    /// Session id.
    pub session_id: String,
    /// Recorded session start (`meta.created_ts`), if the metadata was readable.
    pub created_ts: Option<i64>,
    /// Parsed events, in file order.
    pub events: Vec<AgentEvent>,
    /// Corrupt/unreadable JSONL lines skipped while reading (honesty surface).
    pub skipped_lines: usize,
    /// Per-session usage sidecar, or `None` when unavailable.
    pub usage: Option<SessionUsage>,
}

/// Destructive-class command-flag counts by matcher severity. These are pattern
/// hits over recorded command text, NOT policy judgments or verdicts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct FlagCounts {
    /// Number of `critical`-severity matcher hits.
    pub critical: usize,
    /// Number of `warning`-severity matcher hits.
    pub warning: usize,
}

/// Recorded test/build/lint command activity, aggregated (ADR-0005 / issue #64).
/// These are the cross-session tally of the per-session claim-vs-reality facts:
/// how many test-like commands were recorded and their observed status. Facts
/// only — a `no_result` is an unpaired call (a failed Bash fires no completion
/// hook), surfaced as *outcome not observed*, never as a failure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TestActivity {
    /// Recorded `test`-kind commands.
    pub test: usize,
    /// Recorded `build`-kind commands.
    pub build: usize,
    /// Recorded `lint`-kind commands.
    pub lint: usize,
    /// Of all test-like commands, those with an `ok` result.
    pub ok: usize,
    /// Of all test-like commands, those with a `failed` result.
    pub failed: usize,
    /// Of all test-like commands, those with no paired result (outcome not
    /// observed — never counted as a failure).
    pub no_result: usize,
}

impl TestActivity {
    /// Tally one classified command by kind and observed status.
    fn record(&mut self, kind: &str, status: &str) {
        match kind {
            "test" => self.test += 1,
            "build" => self.build += 1,
            "lint" => self.lint += 1,
            _ => {}
        }
        match status {
            "ok" => self.ok += 1,
            "failed" => self.failed += 1,
            "no-result" => self.no_result += 1,
            _ => {}
        }
    }

    /// Add another activity's counts into this one.
    fn merge(&mut self, other: &TestActivity) {
        self.test += other.test;
        self.build += other.build;
        self.lint += other.lint;
        self.ok += other.ok;
        self.failed += other.failed;
        self.no_result += other.no_result;
    }

    /// Total test-like commands recorded (test + build + lint).
    fn total(&self) -> usize {
        self.test + self.build + self.lint
    }
}

/// Per-model token totals, summed across the sessions that had a usage sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelTokenTotals {
    /// The `message.model` these totals were reported under.
    pub model: String,
    /// Sum of `input_tokens`.
    pub input_tokens: u64,
    /// Sum of `output_tokens`.
    pub output_tokens: u64,
    /// Sum of `cache_creation_input_tokens`.
    pub cache_creation_input_tokens: u64,
    /// Sum of `cache_read_input_tokens`.
    pub cache_read_input_tokens: u64,
}

impl ModelTokenTotals {
    fn zero(model: &str) -> Self {
        Self {
            model: model.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }
    }

    fn add(&mut self, usage: &ModelUsage) {
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .saturating_add(usage.cache_creation_input_tokens);
        self.cache_read_input_tokens = self
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
    }
}

/// One project's aggregated ledger for the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectDigest {
    /// Display name (the basename of the group's cwd path).
    pub name: String,
    /// Full cwd path (the group key), or `"unknown project"`.
    pub path: String,
    /// Whether this is the no-cwd bucket.
    pub is_unknown: bool,
    /// Sessions attributed to this project.
    pub sessions: usize,
    /// Summed session duration (last-minus-first event), milliseconds.
    pub duration_ms: i64,
    /// User prompts (hook-sourced `Prompt` events).
    pub prompts: usize,
    /// Tool calls (`ToolCall` events).
    pub tool_calls: usize,
    /// Executed commands (Bash tool calls).
    pub commands: usize,
    /// Distinct files touched (union of `tool_input.file_path` across sessions).
    pub files_touched: usize,
    /// Destructive-class command-flag counts by matcher severity.
    pub flags: FlagCounts,
    /// Recorded test/build/lint command activity (claim-vs-reality facts).
    pub test_activity: TestActivity,
    /// Per-model token totals across sessions that had a usage sidecar.
    pub per_model: Vec<ModelTokenTotals>,
    /// Distinct models observed with usage, sorted by name.
    pub models_used: Vec<String>,
    /// Sessions attributed here whose usage sidecar was unavailable.
    pub sessions_usage_unavailable: usize,
    /// Sessions attributed here that spanned more than one distinct cwd (they are
    /// attributed to their first observed cwd; disclosed, never silent).
    pub multi_cwd_sessions: usize,
}

/// Window-level totals across all included sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverallTotals {
    /// Number of project groups (including the unknown bucket if present).
    pub projects: usize,
    /// Included sessions.
    pub sessions: usize,
    /// Summed session duration, milliseconds.
    pub duration_ms: i64,
    /// User prompts (hook-sourced).
    pub prompts: usize,
    /// Tool calls.
    pub tool_calls: usize,
    /// Executed commands (Bash).
    pub commands: usize,
    /// Distinct files touched across ALL included sessions (global union).
    pub files_touched: usize,
    /// Command-flag counts by matcher severity.
    pub flags: FlagCounts,
    /// Recorded test/build/lint command activity across all included sessions.
    pub test_activity: TestActivity,
    /// Per-model token totals across all sessions that had usage.
    pub per_model: Vec<ModelTokenTotals>,
    /// Distinct models observed with usage, sorted.
    pub models_used: Vec<String>,
}

/// Honesty surfaces: the states a naive reader might otherwise miss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HonestySurfaces {
    /// Included sessions with no usage sidecar (token totals exclude these).
    pub sessions_usage_unavailable: usize,
    /// Included sessions with no observed cwd (the unknown-project bucket).
    pub unknown_project_sessions: usize,
    /// Included sessions spanning more than one distinct cwd.
    pub multi_cwd_sessions: usize,
    /// Corrupt/unreadable JSONL lines skipped across included sessions.
    pub corrupt_lines: usize,
}

/// The concrete window this digest covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WindowDescriptor {
    /// Which selector produced the window.
    pub kind: WindowKind,
    /// Lower bound, Unix epoch ms. `None` means all recorded history.
    pub since_ms: Option<i64>,
    /// Upper bound (the injected `now_ms`), Unix epoch ms.
    pub now_ms: i64,
}

/// The computed cross-session delegation ledger. Serializes directly as `--json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DigestReport {
    /// The window covered.
    pub window: WindowDescriptor,
    /// Per-project sections, most-active first, unknown bucket last.
    pub projects: Vec<ProjectDigest>,
    /// Window-level totals.
    pub totals: OverallTotals,
    /// Honesty surfaces.
    pub honesty: HonestySurfaces,
}

/// Build the requested [`WindowMode`] from the three (mutually exclusive) flags.
///
/// Pure: parses the `--since` duration (reusing [`parse_relative_ms`]) but reads
/// no clock. Errors if more than one selector is given, or the duration is
/// malformed. `None` of the three → [`WindowMode::All`].
pub fn window_mode_from_flags(today: bool, week: bool, since: Option<&str>) -> Result<WindowMode> {
    let selected = usize::from(today) + usize::from(week) + usize::from(since.is_some());
    if selected > 1 {
        return Err(anyhow!(
            "--today, --week, and --since are mutually exclusive; pass at most one"
        ));
    }
    if today {
        Ok(WindowMode::Today)
    } else if week {
        Ok(WindowMode::Week)
    } else if let Some(dur) = since {
        Ok(WindowMode::Since {
            span_ms: parse_relative_ms(dur)?,
        })
    } else {
        Ok(WindowMode::All)
    }
}

/// Compute the window's lower bound (Unix epoch ms) for the given `now_ms`.
///
/// Pure and deterministic. UTC calendar days are used deliberately (matching the
/// repo's UTC-only time display): a day boundary is exact integer arithmetic on
/// epoch seconds, so no civil-date conversion is needed. `None` (all history)
/// stays `None`.
pub fn window_since_ms(mode: WindowMode, now_ms: i64) -> Option<i64> {
    match mode {
        WindowMode::Today => Some(utc_day_start_ms(now_ms, 0)),
        WindowMode::Week => Some(utc_day_start_ms(now_ms, WEEK_LOOKBACK_DAYS)),
        WindowMode::Since { span_ms } => Some(now_ms.saturating_sub(span_ms)),
        WindowMode::All => None,
    }
}

/// UTC midnight `days_back` calendar days before the day containing `now_ms`.
fn utc_day_start_ms(now_ms: i64, days_back: i64) -> i64 {
    let now_secs = now_ms.div_euclid(MS_PER_SEC);
    let day_index = now_secs.div_euclid(SECS_PER_DAY);
    (day_index - days_back)
        .saturating_mul(SECS_PER_DAY)
        .saturating_mul(MS_PER_SEC)
}

/// Read every session's inputs from the store (I/O boundary only).
///
/// Reads meta (`created_ts`), events, and the usage sidecar per session. A
/// missing/corrupt `usage.json` degrades to `None` (usage unavailable) inside
/// [`SessionStore::read_usage`], so one bad sidecar never fails the whole digest.
pub fn collect_session_inputs(store: &SessionStore) -> Result<Vec<SessionInput>, StoreError> {
    let mut inputs = Vec::new();
    for id in store.list_sessions()? {
        let read = store.read(&id)?;
        let created_ts = store.read_meta(&id).ok().map(|m| m.created_ts);
        let usage = store.read_usage(&id)?;
        inputs.push(SessionInput {
            session_id: id,
            created_ts,
            events: read.events,
            skipped_lines: read.skipped_lines,
            usage,
        });
    }
    Ok(inputs)
}

/// Build the digest from already-read session inputs. Pure: no I/O, no wall-clock.
///
/// A session is INCLUDED iff its start (`created_ts`, else its first event's `ts`)
/// falls at or after `since_ms` (all sessions when `since_ms` is `None`). Included
/// sessions contribute their WHOLE totals — usage sidecars are per-session and
/// cannot be sliced to a sub-window, so partial-window attribution is refused.
pub fn build_digest(
    inputs: &[SessionInput],
    now_ms: i64,
    since_ms: Option<i64>,
    mode: WindowMode,
) -> DigestReport {
    let mut projects: BTreeMap<ProjectKey, ProjectAcc> = BTreeMap::new();
    let mut global_files: BTreeSet<String> = BTreeSet::new();
    let mut global_models: BTreeMap<String, ModelTokenTotals> = BTreeMap::new();
    let mut honesty = HonestySurfaces {
        sessions_usage_unavailable: 0,
        unknown_project_sessions: 0,
        multi_cwd_sessions: 0,
        corrupt_lines: 0,
    };

    for input in inputs {
        if !is_included(session_start_ms(input), since_ms) {
            continue;
        }
        let metrics = session_metrics(input);

        if metrics.is_unknown {
            honesty.unknown_project_sessions += 1;
        }
        if metrics.multi_cwd {
            honesty.multi_cwd_sessions += 1;
        }
        if metrics.usage_unavailable {
            honesty.sessions_usage_unavailable += 1;
        }
        honesty.corrupt_lines += input.skipped_lines;

        for path in &metrics.files {
            global_files.insert(path.clone());
        }
        for usage in &metrics.per_model {
            merge_model(&mut global_models, usage);
        }

        let key = if metrics.is_unknown {
            ProjectKey::Unknown
        } else {
            ProjectKey::Path(metrics.project_key.clone())
        };
        projects
            .entry(key)
            .or_insert_with(|| ProjectAcc::new(&metrics.project_name, &metrics.project_key))
            .add(&metrics);
    }

    let mut project_digests: Vec<ProjectDigest> =
        projects.into_values().map(ProjectAcc::finalize).collect();
    sort_projects(&mut project_digests);

    let totals = overall_totals(&project_digests, &global_files, global_models);

    DigestReport {
        window: WindowDescriptor {
            kind: mode.kind(),
            since_ms,
            now_ms,
        },
        projects: project_digests,
        totals,
        honesty,
    }
}

// --- pure aggregation --------------------------------------------------------

/// Project group key. A dedicated `Unknown` variant keeps the no-cwd bucket from
/// ever colliding with a real path. Derived `Ord` sorts `Path` before `Unknown`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ProjectKey {
    Path(String),
    Unknown,
}

/// Per-session metrics, all derived purely from the session's inputs.
struct SessionMetrics {
    project_key: String,
    project_name: String,
    is_unknown: bool,
    multi_cwd: bool,
    duration_ms: i64,
    prompts: usize,
    tool_calls: usize,
    commands: usize,
    files: Vec<String>,
    flags: FlagCounts,
    test_activity: TestActivity,
    per_model: Vec<ModelUsage>,
    usage_unavailable: bool,
}

/// A session's start: recorded `created_ts`, else its first event's time.
fn session_start_ms(input: &SessionInput) -> Option<i64> {
    input
        .created_ts
        .or_else(|| input.events.first().map(|e| e.ts))
}

/// Whether a session with the given start is inside the window.
///
/// `None` window (all history) always includes. A session whose start cannot be
/// determined (`None`) is excluded from a bounded window: we cannot confirm it
/// falls inside, and honesty forbids attributing it on a guess.
fn is_included(start_ms: Option<i64>, since_ms: Option<i64>) -> bool {
    match since_ms {
        None => true,
        Some(since) => start_ms.is_some_and(|start| start >= since),
    }
}

/// Compute one session's metrics.
fn session_metrics(input: &SessionInput) -> SessionMetrics {
    // Distinct cwds, in first-seen order; the first is the project attribution.
    let mut cwds: Vec<&str> = Vec::new();
    for ev in &input.events {
        if let Some(cwd) = ev.payload.get(FIELD_CWD).and_then(Value::as_str) {
            if !cwds.contains(&cwd) {
                cwds.push(cwd);
            }
        }
    }
    let (project_key, project_name, is_unknown) = match cwds.first() {
        Some(first) => (first.to_string(), basename(first), false),
        None => (
            UNKNOWN_PROJECT.to_string(),
            UNKNOWN_PROJECT.to_string(),
            true,
        ),
    };
    let multi_cwd = cwds.len() > 1;

    let duration_ms = match (input.events.first(), input.events.last()) {
        (Some(first), Some(last)) => (last.ts - first.ts).max(0),
        _ => 0,
    };

    let mut prompts = 0;
    let mut tool_calls = 0;
    let mut commands = 0;
    let mut files: Vec<String> = Vec::new();
    let mut flags = FlagCounts::default();
    for ev in &input.events {
        match ev.kind {
            // Only HOOK-sourced prompts are user prompts. The transcript adapter
            // emits `Prompt` events (assistant prose) with `Source::Transcript`;
            // counting those would over-report user turns.
            EventKind::Prompt if ev.source == Source::Hooks => prompts += 1,
            EventKind::ToolCall => {
                tool_calls += 1;
                if let Some(path) = tool_input_str(ev, FIELD_FILE_PATH) {
                    if !files.iter().any(|f| f == path) {
                        files.push(path.to_string());
                    }
                }
                if ev.payload.get(FIELD_TOOL_NAME).and_then(Value::as_str) == Some(BASH_TOOL) {
                    commands += 1;
                    if let Some(command) = tool_input_str(ev, FIELD_COMMAND) {
                        for flag in flags_for_command(command) {
                            match flag.severity {
                                FlagSeverity::Critical => flags.critical += 1,
                                FlagSeverity::Warning => flags.warning += 1,
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Test/build/lint command facts, from the same classifier the per-session
    // claim-vs-reality panel uses (issue #64). Status is the timeline-paired
    // outcome; `no-result` is never counted as a failure.
    let mut test_activity = TestActivity::default();
    for run in test_command_runs(&input.events) {
        test_activity.record(run.kind, run.status);
    }

    // A missing sidecar is "usage unavailable" (contributes zero, never recorded
    // as zero tokens). A present sidecar with no parseable usage simply has an
    // empty `per_model` — it is available, just empty.
    let (per_model, usage_unavailable) = match &input.usage {
        Some(usage) => (usage.per_model.clone(), false),
        None => (Vec::new(), true),
    };

    SessionMetrics {
        project_key,
        project_name,
        is_unknown,
        multi_cwd,
        duration_ms,
        prompts,
        tool_calls,
        commands,
        files,
        flags,
        test_activity,
        per_model,
        usage_unavailable,
    }
}

/// One project's running accumulation.
struct ProjectAcc {
    name: String,
    path: String,
    is_unknown: bool,
    sessions: usize,
    duration_ms: i64,
    prompts: usize,
    tool_calls: usize,
    commands: usize,
    files: BTreeSet<String>,
    flags: FlagCounts,
    test_activity: TestActivity,
    per_model: BTreeMap<String, ModelTokenTotals>,
    usage_unavailable: usize,
    multi_cwd: usize,
}

impl ProjectAcc {
    fn new(name: &str, path: &str) -> Self {
        Self {
            name: name.to_string(),
            path: path.to_string(),
            is_unknown: path == UNKNOWN_PROJECT,
            sessions: 0,
            duration_ms: 0,
            prompts: 0,
            tool_calls: 0,
            commands: 0,
            files: BTreeSet::new(),
            flags: FlagCounts::default(),
            test_activity: TestActivity::default(),
            per_model: BTreeMap::new(),
            usage_unavailable: 0,
            multi_cwd: 0,
        }
    }

    fn add(&mut self, metrics: &SessionMetrics) {
        self.sessions += 1;
        self.duration_ms = self.duration_ms.saturating_add(metrics.duration_ms);
        self.prompts += metrics.prompts;
        self.tool_calls += metrics.tool_calls;
        self.commands += metrics.commands;
        for path in &metrics.files {
            self.files.insert(path.clone());
        }
        self.flags.critical += metrics.flags.critical;
        self.flags.warning += metrics.flags.warning;
        self.test_activity.merge(&metrics.test_activity);
        for usage in &metrics.per_model {
            merge_model(&mut self.per_model, usage);
        }
        if metrics.usage_unavailable {
            self.usage_unavailable += 1;
        }
        if metrics.multi_cwd {
            self.multi_cwd += 1;
        }
    }

    fn finalize(self) -> ProjectDigest {
        let models_used = self.per_model.keys().cloned().collect();
        ProjectDigest {
            name: self.name,
            path: self.path,
            is_unknown: self.is_unknown,
            sessions: self.sessions,
            duration_ms: self.duration_ms,
            prompts: self.prompts,
            tool_calls: self.tool_calls,
            commands: self.commands,
            files_touched: self.files.len(),
            flags: self.flags,
            test_activity: self.test_activity,
            per_model: self.per_model.into_values().collect(),
            models_used,
            sessions_usage_unavailable: self.usage_unavailable,
            multi_cwd_sessions: self.multi_cwd,
        }
    }
}

/// Merge one model's usage into a per-model totals map.
fn merge_model(map: &mut BTreeMap<String, ModelTokenTotals>, usage: &ModelUsage) {
    map.entry(usage.model.clone())
        .or_insert_with(|| ModelTokenTotals::zero(&usage.model))
        .add(usage);
}

/// Sum the window totals. Scalars are summed from the finalized project digests;
/// distinct files and per-model tokens come from the global union/sum maintained
/// during aggregation (per-project distinct counts cannot be re-summed globally).
fn overall_totals(
    projects: &[ProjectDigest],
    global_files: &BTreeSet<String>,
    global_models: BTreeMap<String, ModelTokenTotals>,
) -> OverallTotals {
    let mut totals = OverallTotals {
        projects: projects.len(),
        sessions: 0,
        duration_ms: 0,
        prompts: 0,
        tool_calls: 0,
        commands: 0,
        files_touched: global_files.len(),
        flags: FlagCounts::default(),
        test_activity: TestActivity::default(),
        per_model: Vec::new(),
        models_used: global_models.keys().cloned().collect(),
    };
    for project in projects {
        totals.sessions += project.sessions;
        totals.duration_ms = totals.duration_ms.saturating_add(project.duration_ms);
        totals.prompts += project.prompts;
        totals.tool_calls += project.tool_calls;
        totals.commands += project.commands;
        totals.flags.critical += project.flags.critical;
        totals.flags.warning += project.flags.warning;
        totals.test_activity.merge(&project.test_activity);
    }
    totals.per_model = global_models.into_values().collect();
    totals
}

/// Order projects most-active first (unknown bucket always last).
fn sort_projects(projects: &mut [ProjectDigest]) {
    projects.sort_by(|a, b| {
        a.is_unknown
            .cmp(&b.is_unknown)
            .then_with(|| b.sessions.cmp(&a.sessions))
            .then_with(|| a.path.cmp(&b.path))
    });
}

/// Read a string field from a tool call's `tool_input`.
fn tool_input_str<'a>(ev: &'a AgentEvent, key: &str) -> Option<&'a str> {
    ev.payload
        .get(FIELD_TOOL_INPUT)
        .and_then(|input| input.get(key))
        .and_then(Value::as_str)
}

/// The last path component of `path`, or the path itself if it has none.
fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string())
}

// --- rendering ---------------------------------------------------------------

/// Serialize the digest as pretty JSON (`--json`), with a trailing newline to
/// match the other commands' convention.
pub fn to_json(report: &DigestReport) -> Result<String, serde_json::Error> {
    let mut out = serde_json::to_string_pretty(report)?;
    out.push('\n');
    Ok(out)
}

/// Render the digest as a shareable markdown ledger. Deterministic function of
/// `report`. FACTS ONLY — no judgment wording anywhere.
pub fn to_markdown(report: &DigestReport) -> String {
    let mut out = String::new();
    push_line(&mut out, "# Delegation digest");
    push_line(&mut out, "");
    push_line(
        &mut out,
        "_A factual, cross-session ledger of what your agents did, grouped by \
         project. Observation only: every number derives from recorded hook \
         events._",
    );
    push_line(&mut out, "");
    push_line(
        &mut out,
        &format!("Digest window: {}", window_sentence(&report.window)),
    );
    push_line(&mut out, "");

    render_totals(&mut out, &report.totals);
    render_projects(&mut out, &report.projects);
    render_honesty(&mut out, &report.honesty);
    render_scope(&mut out);
    out
}

/// The explicit window sentence, e.g. `UTC today, <since> to <now>`.
fn window_sentence(window: &WindowDescriptor) -> String {
    let now = format_utc(window.now_ms);
    match (window.kind, window.since_ms) {
        (WindowKind::Today, Some(since)) => {
            format!("UTC today, {} to {now}", format_utc(since))
        }
        (WindowKind::Week, Some(since)) => {
            format!("last 7 UTC calendar days, {} to {now}", format_utc(since))
        }
        (WindowKind::Since, Some(since)) => {
            format!("since {} to {now} (UTC)", format_utc(since))
        }
        // `All` (or any mode that somehow lacks a lower bound) has no lower bound.
        (_, None) => format!("all recorded history, through {now} (UTC)"),
        (WindowKind::All, Some(since)) => format!("{} to {now} (UTC)", format_utc(since)),
    }
}

fn render_totals(out: &mut String, totals: &OverallTotals) {
    push_line(out, "## Overall totals");
    push_line(out, "");
    push_line(out, &format!("- Projects: {}", totals.projects));
    push_line(out, &format!("- Sessions: {}", totals.sessions));
    push_line(
        out,
        &format!(
            "- Duration (summed wall-span): {}",
            format_duration_ms(totals.duration_ms)
        ),
    );
    push_line(
        out,
        &format!("- Prompts (user, hook-sourced): {}", totals.prompts),
    );
    push_line(out, &format!("- Tool calls: {}", totals.tool_calls));
    push_line(out, &format!("- Commands (Bash): {}", totals.commands));
    push_line(
        out,
        &format!("- Files touched (distinct): {}", totals.files_touched),
    );
    push_line(out, &format!("- {}", flag_line(&totals.flags)));
    push_line(
        out,
        &format!("- {}", test_activity_line(&totals.test_activity)),
    );
    push_line(out, "- Token totals from available usage sidecars:");
    render_model_totals(out, &totals.per_model);
    push_line(out, "");
}

fn render_projects(out: &mut String, projects: &[ProjectDigest]) {
    push_line(out, "## Projects");
    push_line(out, "");
    if projects.is_empty() {
        push_line(out, "_No sessions in this window._");
        push_line(out, "");
        return;
    }
    for project in projects {
        push_line(out, &format!("### {} (`{}`)", project.name, project.path));
        push_line(out, "");
        push_line(out, &format!("- Sessions: {}", project.sessions));
        push_line(
            out,
            &format!(
                "- Duration (summed wall-span): {}",
                format_duration_ms(project.duration_ms)
            ),
        );
        push_line(
            out,
            &format!("- Prompts (user, hook-sourced): {}", project.prompts),
        );
        push_line(out, &format!("- Tool calls: {}", project.tool_calls));
        push_line(out, &format!("- Commands (Bash): {}", project.commands));
        push_line(
            out,
            &format!("- Files touched (distinct): {}", project.files_touched),
        );
        push_line(out, &format!("- {}", flag_line(&project.flags)));
        push_line(
            out,
            &format!("- {}", test_activity_line(&project.test_activity)),
        );
        if !project.models_used.is_empty() {
            push_line(
                out,
                &format!("- Models used: {}", project.models_used.join(", ")),
            );
        }
        push_line(out, "- Token totals from available usage sidecars:");
        render_model_totals(out, &project.per_model);
        push_line(
            out,
            &format!(
                "- Usage unavailable: {} session(s)",
                project.sessions_usage_unavailable
            ),
        );
        push_line(
            out,
            &format!(
                "- Multi-cwd sessions: {} (attributed to their first observed cwd)",
                project.multi_cwd_sessions
            ),
        );
        push_line(out, "");
    }
}

/// A single factual flag line — matcher severity counts, never a verdict.
fn flag_line(flags: &FlagCounts) -> String {
    format!(
        "Command flags by matcher severity: critical {}, warning {}",
        flags.critical, flags.warning
    )
}

/// Facts-only summary of recorded test/build/lint commands and their observed
/// status. `no-result` is an unpaired call (a failed Bash fires no completion
/// hook) — surfaced as such, never as a failure or as "tests did not pass".
fn test_activity_line(activity: &TestActivity) -> String {
    if activity.total() == 0 {
        return "Test-like commands recorded: 0".to_string();
    }
    format!(
        "Test-like commands recorded: {} (test {}, build {}, lint {}) — \
         status: {} ok, {} failed, {} no-result",
        activity.total(),
        activity.test,
        activity.build,
        activity.lint,
        activity.ok,
        activity.failed,
        activity.no_result,
    )
}

/// Render per-model token totals, or a placeholder when none are available.
fn render_model_totals(out: &mut String, per_model: &[ModelTokenTotals]) {
    if per_model.is_empty() {
        push_line(out, "  - _No usage sidecars available._");
        return;
    }
    for model in per_model {
        push_line(
            out,
            &format!(
                "  - {}: input {}, output {}, cache-creation {}, cache-read {}",
                model.model,
                model.input_tokens,
                model.output_tokens,
                model.cache_creation_input_tokens,
                model.cache_read_input_tokens,
            ),
        );
    }
}

fn render_honesty(out: &mut String, honesty: &HonestySurfaces) {
    push_line(out, "## Honesty surfaces");
    push_line(out, "");
    push_line(
        out,
        &format!(
            "- Usage unavailable: {} session(s) — token totals exclude these; an \
             absent sidecar is not zero tokens.",
            honesty.sessions_usage_unavailable
        ),
    );
    push_line(
        out,
        &format!(
            "- Unknown-project sessions: {} — no cwd was observed in the session's \
             events.",
            honesty.unknown_project_sessions
        ),
    );
    push_line(
        out,
        &format!(
            "- Multi-cwd sessions: {} — attributed to their first observed cwd; the \
             count is disclosed per project.",
            honesty.multi_cwd_sessions
        ),
    );
    push_line(
        out,
        &format!(
            "- Corrupt/skipped lines: {} — unreadable JSONL lines across included \
             sessions.",
            honesty.corrupt_lines
        ),
    );
    push_line(out, "");
}

fn render_scope(out: &mut String) {
    push_line(out, "## Scope");
    push_line(out, "");
    push_line(
        out,
        "- This digest is FACTS ONLY: counts, durations, and token totals. It \
         makes no judgment about efficiency, model choice, or command class — \
         that belongs to the agent/audit layer, not the CLI.",
    );
    push_line(
        out,
        "- Command flag counts are destructive-class pattern-matcher hits over \
         recorded command text, not policy judgments or verdicts.",
    );
    push_line(
        out,
        "- Test-like command counts are a best-effort classifier over recorded \
         command text (a runner not in the table is not counted); a `no-result` \
         is an unpaired call whose outcome was not observed, never a failure.",
    );
    push_line(
        out,
        "- A session is included when its start falls inside the window; its whole \
         totals are counted. Usage sidecars are per-session and cannot be sliced \
         to a sub-window.",
    );
    push_line(
        out,
        "- Duration is each session's wall-span (first to last recorded event): it \
         includes idle time between turns, and concurrent sessions' spans overlap, \
         so the summed figure is span coverage, NOT additive time worked (it can \
         exceed real elapsed time).",
    );
    push_line(out, "- All times are UTC.");
}

/// Append one line and a trailing newline.
fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_witness_core::{Attribution, CONFIDENCE_CERTAIN};
    use serde_json::json;
    use tempfile::TempDir;

    // A fixed clock instant: 2023-11-14 22:13:20Z.
    const NOW: i64 = 1_700_000_000_000;
    const DAY_MS: i64 = 86_400_000;

    // --- input builders ---------------------------------------------------

    fn event(ts: i64, source: Source, kind: EventKind, payload: Value) -> AgentEvent {
        AgentEvent::new(
            ts,
            "s",
            source,
            kind,
            Attribution::Direct,
            CONFIDENCE_CERTAIN,
            payload,
        )
    }

    fn hook_prompt(ts: i64, cwd: &str) -> AgentEvent {
        event(
            ts,
            Source::Hooks,
            EventKind::Prompt,
            json!({"prompt": "hi", "cwd": cwd}),
        )
    }

    fn transcript_prompt(ts: i64) -> AgentEvent {
        event(
            ts,
            Source::Transcript,
            EventKind::Prompt,
            json!({"text": "assistant prose"}),
        )
    }

    fn tool_call(ts: i64, payload: Value) -> AgentEvent {
        event(ts, Source::Hooks, EventKind::ToolCall, payload)
    }

    fn bash(ts: i64, command: &str, cwd: &str) -> AgentEvent {
        tool_call(
            ts,
            json!({"tool_name": "Bash", "tool_input": {"command": command}, "cwd": cwd}),
        )
    }

    fn write_file(ts: i64, path: &str, cwd: &str) -> AgentEvent {
        tool_call(
            ts,
            json!({"tool_name": "Write", "tool_input": {"file_path": path}, "cwd": cwd}),
        )
    }

    fn usage_with(model: &str, input: u64, output: u64) -> SessionUsage {
        let line = format!(
            r#"{{"type":"assistant","message":{{"id":"m","model":"{model}","content":[],"usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}"#
        );
        agent_witness_core::aggregate_transcript_usage(&line, "s")
    }

    fn input(id: &str, created_ts: i64, events: Vec<AgentEvent>) -> SessionInput {
        SessionInput {
            session_id: id.to_string(),
            created_ts: Some(created_ts),
            events,
            skipped_lines: 0,
            usage: None,
        }
    }

    fn digest_all(inputs: &[SessionInput]) -> DigestReport {
        build_digest(inputs, NOW, None, WindowMode::All)
    }

    fn project<'a>(report: &'a DigestReport, name: &str) -> &'a ProjectDigest {
        report
            .projects
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no project {name:?} in {:?}", report.projects))
    }

    // --- window boundaries ------------------------------------------------

    #[test]
    fn utc_today_excludes_yesterday_end_and_includes_today_start() {
        // Arrange: now is midday; today's UTC midnight is the lower bound.
        let now = NOW; // 22:13:20Z on 2023-11-14
        let since = window_since_ms(WindowMode::Today, now).unwrap();
        assert_eq!(format_utc(since), "2023-11-14 00:00:00Z");
        // One session starts at 23:59:59Z the previous day, one at 00:00:00Z today.
        let yesterday = input(
            "y",
            since - MS_PER_SEC,
            vec![hook_prompt(since - MS_PER_SEC, "/w/a")],
        );
        let today = input("t", since, vec![hook_prompt(since, "/w/a")]);

        // Act
        let report = build_digest(&[yesterday, today], now, Some(since), WindowMode::Today);

        // Assert: only today's session counts.
        assert_eq!(report.totals.sessions, 1);
        assert_eq!(report.totals.prompts, 1);
    }

    #[test]
    fn utc_week_includes_day_six_and_excludes_day_seven() {
        let now = NOW;
        let since = window_since_ms(WindowMode::Week, now).unwrap();
        // 6 days before 2023-11-14 is 2023-11-08 midnight.
        assert_eq!(format_utc(since), "2023-11-08 00:00:00Z");
        let day_six = input("d6", since, vec![hook_prompt(since, "/w/a")]);
        let day_seven = input(
            "d7",
            since - MS_PER_SEC,
            vec![hook_prompt(since - MS_PER_SEC, "/w/a")],
        );

        let report = build_digest(&[day_six, day_seven], now, Some(since), WindowMode::Week);

        assert_eq!(report.totals.sessions, 1);
    }

    #[test]
    fn since_duration_boundary_is_inclusive_at_the_lower_bound() {
        let now = NOW;
        let mode = window_mode_from_flags(false, false, Some("30d")).unwrap();
        let since = window_since_ms(mode, now).unwrap();
        assert_eq!(since, now - 30 * DAY_MS);
        let at_bound = input("in", since, vec![hook_prompt(since, "/w/a")]);
        let before = input("out", since - 1, vec![hook_prompt(since - 1, "/w/a")]);

        let report = build_digest(&[at_bound, before], now, Some(since), mode);

        assert_eq!(report.totals.sessions, 1);
    }

    #[test]
    fn session_started_before_window_is_excluded_even_with_a_later_event_inside() {
        // Over-attribution guard: inclusion keys on session START, not activity.
        let now = NOW;
        let since = window_since_ms(WindowMode::Today, now).unwrap();
        // created_ts is before the window; a later event lands inside it.
        let session = input(
            "long",
            since - DAY_MS,
            vec![
                hook_prompt(since - DAY_MS, "/w/a"),
                hook_prompt(since + MS_PER_SEC, "/w/a"),
            ],
        );

        let report = build_digest(&[session], now, Some(since), WindowMode::Today);

        assert_eq!(report.totals.sessions, 0);
    }

    #[test]
    fn mutually_exclusive_window_flags_error() {
        assert!(window_mode_from_flags(true, true, None).is_err());
        assert!(window_mode_from_flags(true, false, Some("7d")).is_err());
        assert!(window_mode_from_flags(false, true, Some("7d")).is_err());
        // Exactly one (or none) is fine.
        assert_eq!(
            window_mode_from_flags(true, false, None).unwrap(),
            WindowMode::Today
        );
        assert_eq!(
            window_mode_from_flags(false, false, None).unwrap(),
            WindowMode::All
        );
    }

    // --- grouping ---------------------------------------------------------

    #[test]
    fn sessions_group_by_project_and_no_cwd_lands_in_unknown() {
        // Two sessions in A, one in B, one with no cwd at all.
        let a1 = input("a1", NOW, vec![hook_prompt(NOW, "/w/proj-a")]);
        let a2 = input("a2", NOW, vec![hook_prompt(NOW, "/w/proj-a")]);
        let b1 = input("b1", NOW, vec![hook_prompt(NOW, "/w/proj-b")]);
        let no_cwd = input(
            "n1",
            NOW,
            vec![tool_call(
                NOW,
                json!({"tool_name": "Read", "tool_input": {}}),
            )],
        );

        let report = digest_all(&[a1, a2, b1, no_cwd]);

        assert_eq!(project(&report, "proj-a").sessions, 2);
        assert_eq!(project(&report, "proj-b").sessions, 1);
        let unknown = project(&report, UNKNOWN_PROJECT);
        assert_eq!(unknown.sessions, 1);
        assert!(unknown.is_unknown);
        assert_eq!(report.honesty.unknown_project_sessions, 1);
        // Unknown bucket sorts last.
        assert_eq!(report.projects.last().unwrap().name, UNKNOWN_PROJECT);
    }

    #[test]
    fn multi_cwd_session_is_counted_and_attributed_to_its_first_cwd() {
        let session = input(
            "m",
            NOW,
            vec![
                hook_prompt(NOW, "/w/first"),
                bash(NOW + 1, "ls", "/w/second"),
            ],
        );

        let report = digest_all(&[session]);

        // Attributed to the FIRST cwd, and disclosed as multi-cwd.
        assert_eq!(project(&report, "first").sessions, 1);
        assert_eq!(project(&report, "first").multi_cwd_sessions, 1);
        assert_eq!(report.honesty.multi_cwd_sessions, 1);
        assert!(report.projects.iter().all(|p| p.name != "second"));
    }

    #[test]
    fn distinct_files_are_unioned_across_a_projects_sessions() {
        let s1 = input(
            "s1",
            NOW,
            vec![
                write_file(NOW, "/w/proj/a.rs", "/w/proj"),
                write_file(NOW + 1, "/w/proj/b.rs", "/w/proj"),
            ],
        );
        let s2 = input(
            "s2",
            NOW,
            vec![
                write_file(NOW, "/w/proj/b.rs", "/w/proj"),
                write_file(NOW + 1, "/w/proj/c.rs", "/w/proj"),
            ],
        );

        let report = digest_all(&[s1, s2]);

        // a, b, c distinct across the two sessions (b appears in both).
        assert_eq!(project(&report, "proj").files_touched, 3);
        assert_eq!(report.totals.files_touched, 3);
    }

    // --- prompt source filter ---------------------------------------------

    #[test]
    fn prompt_count_excludes_transcript_sourced_prompt_events() {
        // A session with one hook user-prompt and one transcript assistant-prose
        // Prompt: only the hook prompt counts.
        let session = input(
            "p",
            NOW,
            vec![hook_prompt(NOW, "/w/proj"), transcript_prompt(NOW + 1)],
        );

        let report = digest_all(&[session]);

        assert_eq!(project(&report, "proj").prompts, 1);
        assert_eq!(report.totals.prompts, 1);
    }

    // --- usage availability -----------------------------------------------

    #[test]
    fn session_with_usage_contributes_tokens_and_without_is_unavailable() {
        let with_usage = SessionInput {
            usage: Some(usage_with("claude-fable-5", 100, 200)),
            ..input("with", NOW, vec![hook_prompt(NOW, "/w/proj")])
        };
        let without = input("without", NOW, vec![hook_prompt(NOW, "/w/proj")]);

        let report = digest_all(&[with_usage, without]);

        let proj = project(&report, "proj");
        assert_eq!(proj.sessions, 2);
        // The without-usage session is counted as a session but adds zero tokens.
        assert_eq!(proj.sessions_usage_unavailable, 1);
        assert_eq!(report.honesty.sessions_usage_unavailable, 1);
        assert_eq!(proj.per_model.len(), 1);
        assert_eq!(proj.per_model[0].model, "claude-fable-5");
        assert_eq!(proj.per_model[0].input_tokens, 100);
        assert_eq!(proj.per_model[0].output_tokens, 200);
        assert_eq!(proj.models_used, vec!["claude-fable-5"]);
        // Totals mirror the project token totals here (single project).
        assert_eq!(report.totals.per_model[0].input_tokens, 100);
    }

    #[test]
    fn token_totals_sum_across_sessions_with_usage() {
        let s1 = SessionInput {
            usage: Some(usage_with("m", 10, 20)),
            ..input("s1", NOW, vec![hook_prompt(NOW, "/w/proj")])
        };
        let s2 = SessionInput {
            usage: Some(usage_with("m", 5, 6)),
            ..input("s2", NOW, vec![hook_prompt(NOW, "/w/proj")])
        };

        let report = digest_all(&[s1, s2]);

        let proj = project(&report, "proj");
        assert_eq!(proj.per_model.len(), 1);
        assert_eq!(proj.per_model[0].input_tokens, 15);
        assert_eq!(proj.per_model[0].output_tokens, 26);
    }

    // --- flags ------------------------------------------------------------

    #[test]
    fn destructive_commands_tally_by_severity_per_project() {
        let session = input(
            "f",
            NOW,
            vec![
                bash(NOW, "rm -rf ~/", "/w/proj"),
                bash(NOW + 1, "git push -f", "/w/proj"),
                bash(NOW + 2, "ls", "/w/proj"),
            ],
        );

        let report = digest_all(&[session]);

        let proj = project(&report, "proj");
        assert_eq!(proj.flags.critical, 1);
        assert_eq!(proj.flags.warning, 1);
        assert_eq!(proj.commands, 3);
        assert_eq!(report.totals.flags.critical, 1);
        assert_eq!(report.totals.flags.warning, 1);
    }

    // --- test-like command activity (issue #64) ---------------------------

    #[test]
    fn test_activity_tallies_kind_and_status_per_project_and_overall() {
        let cwd = "/w/proj";
        let events = vec![
            // A paired `cargo test` → ok.
            tool_call(
                NOW,
                json!({"tool_name": "Bash", "tool_use_id": "t1",
                       "tool_input": {"command": "cargo test"}, "cwd": cwd}),
            ),
            event(
                NOW + 1,
                Source::Hooks,
                EventKind::ToolResult,
                json!({"tool_name": "Bash", "tool_use_id": "t1"}),
            ),
            // An unpaired `cargo build` → no-result, never a failure.
            tool_call(
                NOW + 2,
                json!({"tool_name": "Bash", "tool_use_id": "t2",
                       "tool_input": {"command": "cargo build"}, "cwd": cwd}),
            ),
            // A non-test command is not counted.
            bash(NOW + 3, "ls -la", cwd),
        ];
        let report = digest_all(&[input("s", NOW, events)]);

        let proj = project(&report, "proj");
        assert_eq!(proj.test_activity.test, 1);
        assert_eq!(proj.test_activity.build, 1);
        assert_eq!(proj.test_activity.lint, 0);
        assert_eq!(proj.test_activity.ok, 1);
        assert_eq!(proj.test_activity.failed, 0);
        assert_eq!(proj.test_activity.no_result, 1);
        assert_eq!(proj.test_activity.total(), 2);
        // Overall totals mirror the single project.
        assert_eq!(report.totals.test_activity.test, 1);
        assert_eq!(report.totals.test_activity.no_result, 1);

        // The facts surface in the markdown, factually.
        let md = to_markdown(&report);
        assert!(md.contains("Test-like commands recorded: 2 (test 1, build 1, lint 0)"));
        assert!(md.contains("1 ok, 0 failed, 1 no-result"));
    }

    #[test]
    fn no_test_activity_renders_zero() {
        let report = digest_all(&[input("s", NOW, vec![bash(NOW, "ls", "/w/proj")])]);
        assert_eq!(project(&report, "proj").test_activity.total(), 0);
        assert!(to_markdown(&report).contains("Test-like commands recorded: 0"));
    }

    // --- duration ---------------------------------------------------------

    #[test]
    fn duration_is_last_minus_first_event_summed_per_project() {
        let s1 = input(
            "s1",
            NOW,
            vec![
                hook_prompt(NOW, "/w/proj"),
                bash(NOW + 5_000, "ls", "/w/proj"),
            ],
        );
        let s2 = input(
            "s2",
            NOW,
            vec![
                hook_prompt(NOW, "/w/proj"),
                bash(NOW + 3_000, "ls", "/w/proj"),
            ],
        );

        let report = digest_all(&[s1, s2]);

        assert_eq!(project(&report, "proj").duration_ms, 8_000);
        assert_eq!(report.totals.duration_ms, 8_000);
    }

    // --- json / determinism -----------------------------------------------

    #[test]
    fn json_shape_is_deterministic_for_a_fixed_now() {
        let session = SessionInput {
            usage: Some(usage_with("m", 1, 2)),
            ..input("s", NOW, vec![hook_prompt(NOW, "/w/proj")])
        };
        let report = build_digest(&[session], NOW, Some(NOW - DAY_MS), WindowMode::Today);

        let a = to_json(&report).unwrap();
        let b = to_json(&report).unwrap();
        assert_eq!(a, b);

        let parsed: Value = serde_json::from_str(&a).unwrap();
        assert_eq!(parsed["window"]["kind"], json!("today"));
        assert_eq!(parsed["window"]["now_ms"], json!(NOW));
        assert_eq!(parsed["totals"]["sessions"], json!(1));
        assert_eq!(parsed["projects"][0]["name"], json!("proj"));
        assert_eq!(
            parsed["projects"][0]["per_model"][0]["input_tokens"],
            json!(1)
        );
    }

    // --- markdown honesty / no-judgment wording ---------------------------

    #[test]
    fn markdown_states_the_window_and_carries_honesty_surfaces() {
        let session = input("s", NOW, vec![hook_prompt(NOW, "/w/proj")]);
        let report = build_digest(
            &[session],
            NOW,
            Some(NOW - DAY_MS),
            WindowMode::Since { span_ms: DAY_MS },
        );
        let md = to_markdown(&report);

        assert!(md.contains("Digest window: since"));
        assert!(md.contains("## Honesty surfaces"));
        assert!(md.contains("Usage unavailable:"));
        assert!(md.contains("absent sidecar is not zero tokens"));
        assert!(md.contains("Command flags by matcher severity"));
        assert!(md.contains("pattern-matcher hits"));
        assert!(md.contains("FACTS ONLY"));
        // Duration is labelled and caveated as non-additive wall-span (#70).
        assert!(md.contains("Duration (summed wall-span)"));
        assert!(md.contains("NOT additive time worked"));
    }

    #[test]
    fn markdown_never_uses_judgment_wording() {
        // A fully-populated report exercises every rendered branch.
        let session = input(
            "s",
            NOW,
            vec![
                hook_prompt(NOW, "/w/proj"),
                bash(NOW + 1, "rm -rf ~/", "/w/proj"),
                write_file(NOW + 2, "/w/proj/a.rs", "/w/proj"),
            ],
        );
        let with_usage = SessionInput {
            usage: Some(usage_with("m", 1, 2)),
            ..session
        };
        let report = build_digest(&[with_usage], NOW, None, WindowMode::All);
        let md = to_markdown(&report).to_lowercase();

        for forbidden in [
            "waste",
            "wasteful",
            "risk",
            "risky",
            "oversized",
            "dangerous",
        ] {
            assert!(
                !md.contains(forbidden),
                "digest markdown must not contain judgment word {forbidden:?}"
            );
        }
        // "bad" only as a standalone word (avoid false hits inside other words).
        assert!(
            !md.split(|c: char| !c.is_ascii_alphabetic())
                .any(|w| w == "bad"),
            "digest markdown must not use the word 'bad'"
        );
    }

    #[test]
    fn empty_history_renders_without_panic() {
        let report = digest_all(&[]);
        assert_eq!(report.totals.sessions, 0);
        let md = to_markdown(&report);
        assert!(md.contains("_No sessions in this window._"));
    }

    // --- store edge -------------------------------------------------------

    #[test]
    fn collect_session_inputs_reads_events_meta_and_usage() {
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let mut writer = store.open("sess", NOW).unwrap();
        writer.append(&hook_prompt(NOW, "/w/proj")).unwrap();
        writer.append(&bash(NOW + 1, "ls", "/w/proj")).unwrap();
        drop(writer);
        store
            .write_usage(&{
                let mut u = usage_with("m", 7, 8);
                u.session = "sess".to_string();
                u
            })
            .unwrap();

        let inputs = collect_session_inputs(&store).unwrap();

        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].session_id, "sess");
        assert_eq!(inputs[0].created_ts, Some(NOW));
        assert_eq!(inputs[0].events.len(), 2);
        assert!(inputs[0].usage.is_some());

        // And it flows through the aggregator end to end.
        let report = build_digest(&inputs, NOW + 10, None, WindowMode::All);
        assert_eq!(project(&report, "proj").tool_calls, 1);
        assert_eq!(project(&report, "proj").per_model[0].input_tokens, 7);
    }
}
