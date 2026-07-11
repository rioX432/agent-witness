//! `agent-witness inventory`: configured vs actually-used MCP servers and skills
//! — an attack-surface / capability accounting (issue #50).
//!
//! Two sides, deliberately never conflated (Core Value 1 — honest observation):
//!
//! - **Configured** is a *snapshot read now* from config files and skills
//!   directories. It is emphatically **not** a session event: reading a config
//!   file is not an observed agent action, so it never becomes an [`AgentEvent`]
//!   and never touches the store. The whole configured side is computed
//!   ephemerally at invocation time and discarded when the command returns.
//! - **Used** is aggregated from recorded `ToolCall` events across all sessions
//!   (`mcp__<server>__<tool>` calls and `Skill` calls). This is hook-sourced,
//!   direct-attribution evidence.
//!
//! The command reports both, plus the diff, with an honest time base: configured
//! is as-of `now`; used is over an observation window (`[since, now]`, or all
//! recorded history when `--since` is omitted). The diff labels are factual, not
//! judgmental — "configured now, not observed used in <window>" — never a bare
//! "unused" or "prune".
//!
//! [`build_inventory`] is a pure function of already-read inputs, so the window
//! logic and the diff are deterministic and unit-testable with fixtures. The
//! file/dir I/O ([`read_configured_mcp`], [`read_configured_skills`]) and the
//! cross-session store read ([`collect_used`]) are thin, separately testable
//! seams; the current working directory and home directory are resolved at the
//! binary edge and passed in.
//!
//! **Secrets safety.** `~/.claude.json` is large and its `.mcpServers.<name>`
//! values carry env vars and tokens. This module extracts only the object
//! **keys** (server names) and drops the parsed value; no config value is ever
//! retained in a struct, serialized, logged, or printed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use agent_witness_core::{EventKind, SessionStore};
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;

use crate::timefmt::format_utc;

/// Home-relative Claude Code config file holding user-global and per-project MCP
/// server definitions.
const CLAUDE_JSON: &str = ".claude.json";
/// Checked-in per-project MCP server file, read relative to the current dir.
const MCP_JSON: &str = ".mcp.json";
/// Claude Code config directory name (under home for user skills, under the cwd
/// for project skills).
const CLAUDE_DIR: &str = ".claude";
/// Skills sub-directory name under a `.claude` directory.
const SKILLS_DIR: &str = "skills";
/// Marker file whose presence in an immediate subdirectory makes it a skill.
const SKILL_FILE: &str = "SKILL.md";

/// Object key holding MCP server definitions (in both config files).
const FIELD_MCP_SERVERS: &str = "mcpServers";
/// `~/.claude.json` key holding per-project configuration, keyed by cwd string.
const FIELD_PROJECTS: &str = "projects";
/// Tool-call payload field naming the invoked tool.
const FIELD_TOOL_NAME: &str = "tool_name";
/// Tool-call payload field wrapping the tool's arguments.
const FIELD_TOOL_INPUT: &str = "tool_input";
/// `Skill` tool-input field naming the invoked skill.
const FIELD_SKILL: &str = "skill";
/// Tool name of a skill invocation.
const SKILL_TOOL: &str = "Skill";
/// Prefix marking an MCP tool call (`mcp__<server>__<tool>`).
const MCP_PREFIX: &str = "mcp__";
/// Server/tool separator inside an MCP tool name. Server names legitimately
/// contain single `_` and `-`, so the boundary is the FIRST double underscore.
const MCP_SEP: &str = "__";

/// Milliseconds per second.
const MS_PER_SEC: i64 = 1_000;
/// Seconds per minute.
const SECS_PER_MIN: i64 = 60;
/// Seconds per hour.
const SECS_PER_HOUR: i64 = 3_600;
/// Seconds per day.
const SECS_PER_DAY: i64 = 86_400;
/// Days per week (for the `w` duration unit).
const DAYS_PER_WEEK: i64 = 7;

/// Which configured source an item (or read status) came from. Source-tagged so
/// the same server appearing in several places is never double-counted and the
/// output can name exactly where a capability is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfiguredSource {
    /// `~/.claude.json` → `.mcpServers` (user-global).
    McpUserGlobal,
    /// `~/.claude.json` → `.projects[<cwd>].mcpServers` (user, this project).
    McpUserProject,
    /// `<cwd>/.mcp.json` → `.mcpServers` (checked-in project file).
    McpProjectFile,
    /// `~/.claude/skills/*/` (user skills).
    SkillsUser,
    /// `<cwd>/.claude/skills/*/` (project skills).
    SkillsProject,
}

impl ConfiguredSource {
    /// Human-readable label for report prose.
    fn label(self) -> &'static str {
        match self {
            ConfiguredSource::McpUserGlobal => "~/.claude.json .mcpServers (user-global)",
            ConfiguredSource::McpUserProject => {
                "~/.claude.json .projects[cwd].mcpServers (user, this project)"
            }
            ConfiguredSource::McpProjectFile => "./.mcp.json .mcpServers (project file)",
            ConfiguredSource::SkillsUser => "~/.claude/skills/*/ (user)",
            ConfiguredSource::SkillsProject => "./.claude/skills/*/ (project)",
        }
    }
}

/// The read outcome of one configured source. The distinct states keep an
/// unreadable/unparseable source from silently masquerading as "no items"
/// (Core Value 1): `Missing` (an expected-optional file/dir is absent — not an
/// error) is deliberately separate from `Unreadable` (permission/IO) and
/// `ParseFailed` (bad JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SourceStatus {
    /// Read cleanly; contributed `count` item names.
    Read {
        /// Number of item names this source contributed.
        count: usize,
    },
    /// The expected-optional file/dir was absent. Not an error.
    Missing,
    /// The source exists but could not be read (permission/IO). Reason carries
    /// no config values (only the OS error text).
    Unreadable {
        /// Short, value-free reason (OS error text).
        reason: String,
    },
    /// The source was read but its JSON did not parse. Reason carries no config
    /// values (serde reports position, not content).
    ParseFailed {
        /// Short, value-free reason (parser error text).
        reason: String,
    },
}

impl SourceStatus {
    /// Whether this status represents a source that could not be read (either
    /// unreadable or parse-failed) — the states that make the diff incomplete.
    fn is_broken(&self) -> bool {
        matches!(
            self,
            SourceStatus::Unreadable { .. } | SourceStatus::ParseFailed { .. }
        )
    }
}

/// One configured source's read result: its origin, status, and the item names
/// it contributed. Item names ONLY — never config values (secrets), by
/// construction (this struct has no value field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceRead {
    /// Which source this is.
    pub source: ConfiguredSource,
    /// Read outcome.
    pub status: SourceStatus,
    /// Item names contributed, sorted. Empty unless `status` is `Read`.
    pub names: Vec<String>,
}

/// One observed use of an MCP server or skill: its name and the event time.
/// Produced from the store (one per matching `ToolCall`); the window filter and
/// aggregation happen inside [`build_inventory`] so the window logic stays pure
/// and testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsedObservation {
    /// Server or skill name.
    pub name: String,
    /// Event time, Unix epoch milliseconds.
    pub ts_ms: i64,
}

/// A configured item (server or skill) and the source(s) that define it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfiguredItem {
    /// Item name.
    pub name: String,
    /// Sources that configure it (a server may be configured in several).
    pub sources: Vec<ConfiguredSource>,
}

/// An observed-used item, aggregated over the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsedItem {
    /// Server or skill name.
    pub name: String,
    /// Number of observed calls in the window.
    pub call_count: usize,
    /// Most recent observed call time, Unix epoch milliseconds.
    pub last_used_ms: i64,
}

/// The configured/used/diff report for one category (MCP servers or skills).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CategoryReport {
    /// Per-source read status + contributed names (honest source accounting).
    pub sources: Vec<SourceRead>,
    /// Distinct configured items (union across sources), name-sorted.
    pub configured: Vec<ConfiguredItem>,
    /// Observed-used items within the window, most-used first then name.
    pub used: Vec<UsedItem>,
    /// Configured now, with no observed use in the window (name-sorted).
    pub configured_not_used: Vec<ConfiguredItem>,
    /// Observed used in the window, not in the current configured set. Neutral
    /// facts (dynamic/UUID/claude.ai-connected/since-removed), not misconfig.
    pub used_not_configured: Vec<UsedItem>,
}

/// Whether every configured source read cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Completeness {
    /// Every configured source (MCP and skills) read cleanly.
    Complete,
    /// At least one configured source was unreadable or failed to parse; the
    /// diff may be incomplete.
    Partial,
}

/// The computed inventory: configured snapshot, observed-used window, and diff.
/// Serializes directly as the `--json` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InventoryReport {
    /// Snapshot instant the configured side was read as-of, Unix epoch ms.
    pub now_ms: i64,
    /// Lower bound of the observed-used window, Unix epoch ms. `None` means all
    /// recorded history (no lower bound).
    pub since_ms: Option<i64>,
    /// Whether every configured source read cleanly.
    pub completeness: Completeness,
    /// MCP servers: configured, used, and the diff.
    pub mcp: CategoryReport,
    /// Skills: configured, used, and the diff.
    pub skills: CategoryReport,
}

/// Extract the server name from an MCP tool name (`mcp__<server>__<tool>`).
///
/// Strips the `mcp__` prefix, then takes everything before the FIRST double
/// underscore as the server — server names legitimately contain single `_` and
/// `-` (`mobile-mcp`, `claude_ai_Notion`, UUIDs). A malformed name (no server,
/// or no `server__tool` boundary, e.g. `mcp__` or `mcp__srv`) yields `None` so
/// the caller skips it rather than panicking. Returns `None` for non-MCP tools.
pub fn mcp_server_from_tool(tool_name: &str) -> Option<&str> {
    let rest = tool_name.strip_prefix(MCP_PREFIX)?;
    let (server, _tool) = rest.split_once(MCP_SEP)?;
    (!server.is_empty()).then_some(server)
}

/// Parse a relative duration like `30d`, `7d`, `24h`, `90m`, `45s`, `2w` into a
/// span in milliseconds. Accepts a non-negative integer followed by exactly one
/// unit character (`s`/`m`/`h`/`d`/`w`). Overflow saturates (an absurd span just
/// widens the window to all history). Pure and deterministic.
pub fn parse_relative_ms(input: &str) -> Result<i64> {
    let s = input.trim();
    let invalid = || {
        anyhow!(
            "invalid --since value {input:?}: expected a number followed by a unit \
             (s, m, h, d, w), e.g. 30d"
        )
    };
    // The number is the leading digit run; the unit is the remainder.
    let unit_start = s.find(|c: char| !c.is_ascii_digit()).ok_or_else(invalid)?;
    if unit_start == 0 {
        return Err(invalid()); // No leading number.
    }
    let (num_str, unit) = s.split_at(unit_start);
    let value: i64 = num_str.parse().map_err(|_| invalid())?;
    let secs = match unit {
        "s" => value,
        "m" => value.saturating_mul(SECS_PER_MIN),
        "h" => value.saturating_mul(SECS_PER_HOUR),
        "d" => value.saturating_mul(SECS_PER_DAY),
        "w" => value
            .saturating_mul(SECS_PER_DAY)
            .saturating_mul(DAYS_PER_WEEK),
        _ => return Err(invalid()),
    };
    Ok(secs.saturating_mul(MS_PER_SEC))
}

/// Read the three configured MCP sources for `home`/`cwd`, source-tagged.
///
/// Order: user-global, user-project (both from `~/.claude.json`), then the
/// checked-in `<cwd>/.mcp.json`. Secrets safety: only the `.mcpServers` object
/// keys are extracted; the parsed value (which holds env vars / tokens) is
/// dropped and never retained.
pub fn read_configured_mcp(home: &Path, cwd: &Path) -> Vec<SourceRead> {
    let cwd_key = cwd.to_string_lossy();
    let (global, project) = read_claude_json_mcp(&home.join(CLAUDE_JSON), &cwd_key);
    let project_file = read_mcp_json(&cwd.join(MCP_JSON));
    vec![global, project, project_file]
}

/// Read the two configured skill sources for `home`/`cwd`, source-tagged: user
/// skills under `~/.claude/skills/`, then project skills under
/// `<cwd>/.claude/skills/`.
pub fn read_configured_skills(home: &Path, cwd: &Path) -> Vec<SourceRead> {
    let user_root = home.join(CLAUDE_DIR).join(SKILLS_DIR);
    let project_root = cwd.join(CLAUDE_DIR).join(SKILLS_DIR);
    vec![
        read_skills_source(ConfiguredSource::SkillsUser, &user_root),
        read_skills_source(ConfiguredSource::SkillsProject, &project_root),
    ]
}

/// Scan every recorded session for used MCP servers and skills.
///
/// I/O boundary only: reads each session and emits one [`UsedObservation`] per
/// matching `ToolCall` (an `mcp__…` call, or a `Skill` call carrying
/// `tool_input.skill`). Malformed tool names and `Skill` calls with no skill
/// field are skipped, not counted. Aggregation and the window filter happen in
/// [`build_inventory`].
pub fn collect_used(store: &SessionStore) -> Result<(Vec<UsedObservation>, Vec<UsedObservation>)> {
    let mut mcp = Vec::new();
    let mut skills = Vec::new();
    for id in store.list_sessions()? {
        let read = store.read(&id)?;
        for event in &read.events {
            if event.kind != EventKind::ToolCall {
                continue;
            }
            let Some(tool_name) = event.payload.get(FIELD_TOOL_NAME).and_then(Value::as_str) else {
                continue;
            };
            if let Some(server) = mcp_server_from_tool(tool_name) {
                mcp.push(UsedObservation {
                    name: server.to_string(),
                    ts_ms: event.ts,
                });
            } else if tool_name == SKILL_TOOL {
                if let Some(skill) = event
                    .payload
                    .get(FIELD_TOOL_INPUT)
                    .and_then(|input| input.get(FIELD_SKILL))
                    .and_then(Value::as_str)
                {
                    skills.push(UsedObservation {
                        name: skill.to_string(),
                        ts_ms: event.ts,
                    });
                }
            }
        }
    }
    Ok((mcp, skills))
}

/// Build the inventory report from already-read inputs. Pure: no I/O, no
/// wall-clock. `since_ms` (if set) filters ONLY the used observations
/// (`ts_ms >= since_ms`); the configured side is always the full snapshot.
pub fn build_inventory(
    configured_mcp: Vec<SourceRead>,
    configured_skills: Vec<SourceRead>,
    used_mcp: Vec<UsedObservation>,
    used_skills: Vec<UsedObservation>,
    now_ms: i64,
    since_ms: Option<i64>,
) -> InventoryReport {
    let mcp = build_category(configured_mcp, used_mcp, since_ms);
    let skills = build_category(configured_skills, used_skills, since_ms);
    let completeness = if category_complete(&mcp.sources) && category_complete(&skills.sources) {
        Completeness::Complete
    } else {
        Completeness::Partial
    };
    InventoryReport {
        now_ms,
        since_ms,
        completeness,
        mcp,
        skills,
    }
}

/// Serialize a report as pretty JSON (`--json`), matching the `report` command's
/// trailing-newline convention.
pub fn to_json(report: &InventoryReport) -> Result<String, serde_json::Error> {
    let mut out = serde_json::to_string_pretty(report)?;
    out.push('\n');
    Ok(out)
}

// --- internal aggregation -------------------------------------------------

/// Assemble one category's configured/used/diff view.
fn build_category(
    sources: Vec<SourceRead>,
    used_obs: Vec<UsedObservation>,
    since_ms: Option<i64>,
) -> CategoryReport {
    let configured = union_configured(&sources);
    let used = aggregate_used(used_obs, since_ms);

    let configured_names: BTreeSet<&str> = configured.iter().map(|c| c.name.as_str()).collect();
    let used_names: BTreeSet<&str> = used.iter().map(|u| u.name.as_str()).collect();

    let configured_not_used = configured
        .iter()
        .filter(|c| !used_names.contains(c.name.as_str()))
        .cloned()
        .collect();
    let used_not_configured = used
        .iter()
        .filter(|u| !configured_names.contains(u.name.as_str()))
        .cloned()
        .collect();

    CategoryReport {
        sources,
        configured,
        used,
        configured_not_used,
        used_not_configured,
    }
}

/// Distinct configured items across all sources, name-sorted, each tagged with
/// the source(s) that define it. Broken/missing sources contribute no names
/// (their `names` are empty), so they naturally drop out of the union.
fn union_configured(sources: &[SourceRead]) -> Vec<ConfiguredItem> {
    let mut map: BTreeMap<String, Vec<ConfiguredSource>> = BTreeMap::new();
    for src in sources {
        for name in &src.names {
            let entry = map.entry(name.clone()).or_default();
            if !entry.contains(&src.source) {
                entry.push(src.source);
            }
        }
    }
    map.into_iter()
        .map(|(name, sources)| ConfiguredItem { name, sources })
        .collect()
}

/// Aggregate used observations into per-name call counts + last-used, applying
/// the window filter. Sorted most-used first, ties broken by name.
fn aggregate_used(observations: Vec<UsedObservation>, since_ms: Option<i64>) -> Vec<UsedItem> {
    let mut map: BTreeMap<String, (usize, i64)> = BTreeMap::new();
    for UsedObservation { name, ts_ms } in observations {
        if let Some(since) = since_ms {
            if ts_ms < since {
                continue;
            }
        }
        let entry = map.entry(name).or_insert((0, ts_ms));
        entry.0 += 1;
        entry.1 = entry.1.max(ts_ms);
    }
    let mut items: Vec<UsedItem> = map
        .into_iter()
        .map(|(name, (call_count, last_used_ms))| UsedItem {
            name,
            call_count,
            last_used_ms,
        })
        .collect();
    items.sort_by(|a, b| {
        b.call_count
            .cmp(&a.call_count)
            .then_with(|| a.name.cmp(&b.name))
    });
    items
}

/// Whether none of a category's sources is unreadable/parse-failed.
fn category_complete(sources: &[SourceRead]) -> bool {
    sources.iter().all(|s| !s.status.is_broken())
}

// --- config file / directory readers -------------------------------------

/// Read the user-global and per-project MCP sources out of one `~/.claude.json`.
/// A single file read yields both sources: a missing/unreadable/unparseable file
/// applies the same non-Read status to both, so neither is ever silently empty.
fn read_claude_json_mcp(path: &Path, cwd_key: &str) -> (SourceRead, SourceRead) {
    let both = |status: SourceStatus| {
        (
            SourceRead {
                source: ConfiguredSource::McpUserGlobal,
                status: status.clone(),
                names: Vec::new(),
            },
            SourceRead {
                source: ConfiguredSource::McpUserProject,
                status,
                names: Vec::new(),
            },
        )
    };

    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return both(SourceStatus::Missing),
        Err(e) => {
            return both(SourceStatus::Unreadable {
                reason: e.to_string(),
            })
        }
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(e) => {
            return both(SourceStatus::ParseFailed {
                reason: e.to_string(),
            })
        }
    };

    // Keys only — the parsed `value` (which holds server env/tokens) is dropped
    // at the end of this function and never retained.
    let global_names = mcp_server_keys(value.get(FIELD_MCP_SERVERS));
    let project_names = mcp_server_keys(
        value
            .get(FIELD_PROJECTS)
            .and_then(|projects| projects.get(cwd_key))
            .and_then(|project| project.get(FIELD_MCP_SERVERS)),
    );
    (
        read_source(ConfiguredSource::McpUserGlobal, global_names),
        read_source(ConfiguredSource::McpUserProject, project_names),
    )
}

/// Read the checked-in `<cwd>/.mcp.json` MCP source (keys only).
fn read_mcp_json(path: &Path) -> SourceRead {
    let status_only = |status: SourceStatus| SourceRead {
        source: ConfiguredSource::McpProjectFile,
        status,
        names: Vec::new(),
    };
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return status_only(SourceStatus::Missing)
        }
        Err(e) => {
            return status_only(SourceStatus::Unreadable {
                reason: e.to_string(),
            })
        }
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => read_source(
            ConfiguredSource::McpProjectFile,
            mcp_server_keys(value.get(FIELD_MCP_SERVERS)),
        ),
        Err(e) => status_only(SourceStatus::ParseFailed {
            reason: e.to_string(),
        }),
    }
}

/// Extract the sorted KEYS of an `.mcpServers` object. Values are never touched:
/// this returns names only, so no secret env var or token can leak downstream.
/// A missing or non-object section yields no names.
fn mcp_server_keys(section: Option<&Value>) -> Vec<String> {
    let Some(object) = section.and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut keys: Vec<String> = object.keys().cloned().collect();
    keys.sort();
    keys
}

/// Enumerate immediate subdirectories of `skills_root` that contain a `SKILL.md`.
/// A missing directory is `Missing` (expected-optional), a read error is
/// `Unreadable` — never silently empty.
fn read_skills_source(source: ConfiguredSource, skills_root: &Path) -> SourceRead {
    let entries = match fs::read_dir(skills_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return SourceRead {
                source,
                status: SourceStatus::Missing,
                names: Vec::new(),
            }
        }
        Err(e) => {
            return SourceRead {
                source,
                status: SourceStatus::Unreadable {
                    reason: e.to_string(),
                },
                names: Vec::new(),
            }
        }
    };

    let mut names = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                // A per-entry read error means we cannot fully enumerate the
                // directory; surface it rather than under-report.
                return SourceRead {
                    source,
                    status: SourceStatus::Unreadable {
                        reason: e.to_string(),
                    },
                    names: Vec::new(),
                };
            }
        };
        // `<entry>/SKILL.md` being a file implies `<entry>` is a directory; a
        // non-stattable entry (e.g. a broken symlink) simply is not a skill dir.
        if entry.path().join(SKILL_FILE).is_file() {
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    read_source(source, names)
}

/// Build a clean-read `SourceRead` from a source tag and its item names.
fn read_source(source: ConfiguredSource, names: Vec<String>) -> SourceRead {
    SourceRead {
        status: SourceStatus::Read { count: names.len() },
        source,
        names,
    }
}

// --- markdown rendering ---------------------------------------------------

/// Render the inventory as a shareable markdown document. Deterministic function
/// of `report` (the same report always renders the same bytes).
pub fn to_markdown(report: &InventoryReport) -> String {
    let mut out = String::new();
    push_line(&mut out, "# Capability inventory");
    push_line(&mut out, "");
    push_line(
        &mut out,
        "_Configured vs observed-used MCP servers and skills. Observation only: \
         configured is a snapshot read from config files now; used is ToolCall \
         evidence from recorded sessions._",
    );
    push_line(&mut out, "");
    push_line(
        &mut out,
        &format!("- Configured snapshot read: {}", format_utc(report.now_ms)),
    );
    push_line(
        &mut out,
        &format!("- Observed-use window: {}", window_sentence(report)),
    );
    push_line(
        &mut out,
        &format!(
            "- Completeness: {}",
            completeness_sentence(report.completeness)
        ),
    );
    push_line(&mut out, "");

    render_category(
        &mut out,
        "MCP servers",
        "server",
        &report.mcp,
        report.since_ms,
    );
    render_category(&mut out, "Skills", "skill", &report.skills, report.since_ms);
    render_scope(&mut out, report.completeness);
    out
}

/// Render one category (MCP servers or skills). `noun` is the singular item word
/// used in the factual summary line ("server" / "skill").
fn render_category(
    out: &mut String,
    title: &str,
    noun: &str,
    category: &CategoryReport,
    since_ms: Option<i64>,
) {
    let phrase = window_phrase(since_ms);
    push_line(out, &format!("## {title}"));
    push_line(out, "");

    push_line(out, "### Configured sources");
    push_line(out, "");
    for src in &category.sources {
        push_line(
            out,
            &format!("- {}: {}", src.source.label(), status_phrase(&src.status)),
        );
    }
    push_line(out, "");

    push_line(out, "### Configured now");
    push_line(out, "");
    render_configured_items(out, &category.configured);
    push_line(out, "");

    push_line(out, &format!("### Observed used {phrase}"));
    push_line(out, "");
    render_used_items(out, &category.used);
    push_line(out, "");

    push_line(
        out,
        &format!("### Configured now, not observed used {phrase}"),
    );
    push_line(out, "");
    render_configured_items(out, &category.configured_not_used);
    // One clearly-advisory line — the section header itself stays factual.
    if !category.configured_not_used.is_empty() {
        let names: Vec<&str> = category
            .configured_not_used
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        push_line(out, "");
        push_line(out, &format!("Review for pruning: {}", names.join(", ")));
    }
    push_line(out, "");

    push_line(
        out,
        &format!("### Observed used {phrase}, not configured now"),
    );
    push_line(out, "");
    push_line(
        out,
        "_Neutral facts — dynamic/UUID-named or claude.ai-connected servers, or \
         since-removed config. Not misconfigurations._",
    );
    push_line(out, "");
    render_used_items(out, &category.used_not_configured);
    push_line(out, "");

    // Factual summary line (counts only; no judgment).
    push_line(
        out,
        &format!(
            "{configured} {noun}(s) configured now; {used} observed used {phrase}; \
             {unused} configured {noun}(s) had no observed calls {phrase}.",
            configured = category.configured.len(),
            used = category.used.len(),
            unused = category.configured_not_used.len(),
        ),
    );
    push_line(out, "");
}

/// Render a configured-item list, or a placeholder when empty.
fn render_configured_items(out: &mut String, items: &[ConfiguredItem]) {
    if items.is_empty() {
        push_line(out, "_None._");
        return;
    }
    for item in items {
        let sources: Vec<&str> = item.sources.iter().map(|s| s.label()).collect();
        push_line(
            out,
            &format!("- {} (sources: {})", item.name, sources.join("; ")),
        );
    }
}

/// Render a used-item list, or a placeholder when empty.
fn render_used_items(out: &mut String, items: &[UsedItem]) {
    if items.is_empty() {
        push_line(out, "_None._");
        return;
    }
    for item in items {
        push_line(
            out,
            &format!(
                "- {} — {} call(s), last used {}",
                item.name,
                item.call_count,
                format_utc(item.last_used_ms),
            ),
        );
    }
}

/// The observation-scope / gaps note (always emitted; honesty framing).
fn render_scope(out: &mut String, completeness: Completeness) {
    push_line(out, "## Scope & gaps");
    push_line(out, "");
    push_line(
        out,
        "- Configured is read from config files at invocation time — it is not a \
         recorded session event and is never attributed as an agent action.",
    );
    push_line(
        out,
        "- Observed use is direct-attribution evidence (Claude Code hook tool calls).",
    );
    push_line(
        out,
        "- MCP server config values (env vars, tokens) are never read — only \
         server names.",
    );
    push_line(
        out,
        "- Plugin-provided skills (via enabledPlugins) are not enumerated here.",
    );
    push_line(
        out,
        "- Items observed used but not in the current configured set are neutral \
         historical/dynamic/external evidence, not misconfigurations.",
    );
    if completeness == Completeness::Partial {
        push_line(
            out,
            "- One or more configured sources were unreadable or failed to parse; \
             the configured set (and therefore the diff) may be incomplete.",
        );
    }
}

/// Header sentence describing the observed-use window.
fn window_sentence(report: &InventoryReport) -> String {
    match report.since_ms {
        Some(since) => format!(
            "observed ToolCall events in [{}, {}]",
            format_utc(since),
            format_utc(report.now_ms),
        ),
        None => format!(
            "all recorded history (through {})",
            format_utc(report.now_ms)
        ),
    }
}

/// Short window phrase used in section headers and the summary line.
fn window_phrase(since_ms: Option<i64>) -> String {
    match since_ms {
        Some(since) => format!("since {}", format_utc(since)),
        None => "in all recorded history".to_string(),
    }
}

/// Completeness sentence for the header.
fn completeness_sentence(completeness: Completeness) -> &'static str {
    match completeness {
        Completeness::Complete => "complete (every configured source read cleanly)",
        Completeness::Partial => {
            "partial (a configured source was unreadable or failed to parse; \
             the diff may be incomplete)"
        }
    }
}

/// Human phrase for a source's read status.
fn status_phrase(status: &SourceStatus) -> String {
    match status {
        SourceStatus::Read { count } => format!("read ({count})"),
        SourceStatus::Missing => "missing (optional; absent)".to_string(),
        SourceStatus::Unreadable { reason } => format!("unreadable: {reason}"),
        SourceStatus::ParseFailed { reason } => format!("parse failed: {reason}"),
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
    use agent_witness_core::{AgentEvent, Attribution, Source, CONFIDENCE_CERTAIN};
    use serde_json::json;
    use tempfile::TempDir;

    const NOW: i64 = 1_700_000_000_000;
    const DAY_MS: i64 = 86_400_000;

    // --- helpers ----------------------------------------------------------

    fn read_source_of(source: ConfiguredSource, names: &[&str]) -> SourceRead {
        SourceRead {
            source,
            status: SourceStatus::Read { count: names.len() },
            names: names.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn obs(name: &str, ts_ms: i64) -> UsedObservation {
        UsedObservation {
            name: name.to_string(),
            ts_ms,
        }
    }

    fn tool_call(ts: i64, payload: Value) -> AgentEvent {
        AgentEvent::new(
            ts,
            "s",
            Source::Hooks,
            EventKind::ToolCall,
            Attribution::Direct,
            CONFIDENCE_CERTAIN,
            payload,
        )
    }

    // --- pure aggregator --------------------------------------------------

    #[test]
    fn configured_but_never_used_lands_in_configured_not_used() {
        // Arrange: one MCP server configured, no usage anywhere.
        let mcp = vec![read_source_of(ConfiguredSource::McpUserGlobal, &["redash"])];
        // Act
        let report = build_inventory(mcp, vec![], vec![], vec![], NOW, None);
        // Assert
        assert_eq!(report.mcp.configured.len(), 1);
        assert_eq!(report.mcp.used.len(), 0);
        assert_eq!(report.mcp.configured_not_used.len(), 1);
        assert_eq!(report.mcp.configured_not_used[0].name, "redash");
        assert!(report.mcp.used_not_configured.is_empty());
        assert_eq!(report.completeness, Completeness::Complete);
    }

    #[test]
    fn used_but_not_configured_lands_in_used_not_configured_neutrally() {
        // Arrange: a UUID-named server used, nothing configured.
        let used = vec![obs("some-uuid-server", NOW)];
        // Act
        let report = build_inventory(vec![], vec![], used, vec![], NOW, None);
        // Assert
        assert!(report.mcp.configured.is_empty());
        assert_eq!(report.mcp.used_not_configured.len(), 1);
        assert_eq!(report.mcp.used_not_configured[0].name, "some-uuid-server");
        assert!(report.mcp.configured_not_used.is_empty());
    }

    #[test]
    fn configured_and_used_appears_in_neither_diff() {
        let mcp = vec![read_source_of(ConfiguredSource::McpUserGlobal, &["cmux"])];
        let used = vec![obs("cmux", NOW), obs("cmux", NOW - 10)];
        let report = build_inventory(mcp, vec![], used, vec![], NOW, None);
        assert_eq!(report.mcp.configured.len(), 1);
        assert_eq!(report.mcp.used.len(), 1);
        assert_eq!(report.mcp.used[0].call_count, 2);
        assert!(report.mcp.configured_not_used.is_empty());
        assert!(report.mcp.used_not_configured.is_empty());
    }

    #[test]
    fn everything_empty_yields_an_empty_complete_report() {
        let report = build_inventory(vec![], vec![], vec![], vec![], NOW, None);
        assert!(report.mcp.configured.is_empty());
        assert!(report.mcp.used.is_empty());
        assert!(report.skills.configured.is_empty());
        assert!(report.skills.used.is_empty());
        assert_eq!(report.completeness, Completeness::Complete);
    }

    #[test]
    fn window_filter_excludes_usage_before_since() {
        // Arrange: "old" used 40d ago, "fresh" used now. Window = last 30d.
        let used = vec![obs("old", NOW - 40 * DAY_MS), obs("fresh", NOW)];
        let since = NOW - 30 * DAY_MS;
        // Act
        let report = build_inventory(vec![], vec![], used, vec![], NOW, Some(since));
        // Assert: only "fresh" survives the window.
        assert_eq!(report.mcp.used.len(), 1);
        assert_eq!(report.mcp.used[0].name, "fresh");
    }

    #[test]
    fn used_counts_and_last_used_are_correct() {
        // Arrange: three calls, last one the most recent.
        let used = vec![
            obs("srv", NOW - 100),
            obs("srv", NOW - 50),
            obs("srv", NOW - 10),
        ];
        // Act
        let report = build_inventory(vec![], vec![], used, vec![], NOW, None);
        // Assert
        assert_eq!(report.mcp.used.len(), 1);
        assert_eq!(report.mcp.used[0].call_count, 3);
        assert_eq!(report.mcp.used[0].last_used_ms, NOW - 10);
    }

    #[test]
    fn used_is_sorted_by_call_count_desc_then_name() {
        let used = vec![
            obs("a", NOW),
            obs("b", NOW),
            obs("b", NOW),
            obs("c", NOW),
            obs("c", NOW),
        ];
        let report = build_inventory(vec![], vec![], used, vec![], NOW, None);
        let order: Vec<&str> = report.mcp.used.iter().map(|u| u.name.as_str()).collect();
        // b and c both have 2 calls (name tiebreak → b before c), then a with 1.
        assert_eq!(order, vec!["b", "c", "a"]);
    }

    #[test]
    fn a_server_configured_in_two_sources_lists_both() {
        let mcp = vec![
            read_source_of(ConfiguredSource::McpUserGlobal, &["shared"]),
            read_source_of(ConfiguredSource::McpProjectFile, &["shared"]),
        ];
        let report = build_inventory(mcp, vec![], vec![], vec![], NOW, None);
        assert_eq!(report.mcp.configured.len(), 1);
        assert_eq!(
            report.mcp.configured[0].sources,
            vec![
                ConfiguredSource::McpUserGlobal,
                ConfiguredSource::McpProjectFile
            ]
        );
    }

    #[test]
    fn a_parse_failed_source_flips_completeness_to_partial() {
        let mcp = vec![SourceRead {
            source: ConfiguredSource::McpProjectFile,
            status: SourceStatus::ParseFailed {
                reason: "bad json".to_string(),
            },
            names: Vec::new(),
        }];
        let report = build_inventory(mcp, vec![], vec![], vec![], NOW, None);
        assert_eq!(report.completeness, Completeness::Partial);
    }

    #[test]
    fn a_missing_source_does_not_flip_completeness() {
        let mcp = vec![SourceRead {
            source: ConfiguredSource::McpProjectFile,
            status: SourceStatus::Missing,
            names: Vec::new(),
        }];
        let report = build_inventory(mcp, vec![], vec![], vec![], NOW, None);
        assert_eq!(report.completeness, Completeness::Complete);
    }

    // --- MCP tool_name parser --------------------------------------------

    #[test]
    fn mcp_parser_handles_hyphens_underscores_uuids_and_malformed() {
        assert_eq!(
            mcp_server_from_tool("mcp__mobile-mcp__mobile_take_screenshot"),
            Some("mobile-mcp")
        );
        assert_eq!(
            mcp_server_from_tool("mcp__claude_ai_Notion__notion-fetch"),
            Some("claude_ai_Notion")
        );
        assert_eq!(
            mcp_server_from_tool("mcp__9f8c1e2a-3b4d-4e5f-8a9b-0c1d2e3f4a5b__do"),
            Some("9f8c1e2a-3b4d-4e5f-8a9b-0c1d2e3f4a5b")
        );
        // Malformed: no server / no server__tool boundary → skipped.
        assert_eq!(mcp_server_from_tool("mcp__"), None);
        assert_eq!(mcp_server_from_tool("mcp__srv"), None);
        assert_eq!(mcp_server_from_tool("mcp____tool"), None);
        // Non-MCP tool names are ignored.
        assert_eq!(mcp_server_from_tool("Bash"), None);
        assert_eq!(mcp_server_from_tool("Skill"), None);
    }

    // --- since parser -----------------------------------------------------

    #[test]
    fn parse_relative_ms_accepts_each_unit() {
        assert_eq!(parse_relative_ms("45s").unwrap(), 45 * 1_000);
        assert_eq!(parse_relative_ms("90m").unwrap(), 90 * 60 * 1_000);
        assert_eq!(parse_relative_ms("24h").unwrap(), 24 * 3_600 * 1_000);
        assert_eq!(parse_relative_ms("30d").unwrap(), 30 * 86_400 * 1_000);
        assert_eq!(parse_relative_ms("2w").unwrap(), 2 * 7 * 86_400 * 1_000);
        assert_eq!(parse_relative_ms(" 7d ").unwrap(), 7 * 86_400 * 1_000);
    }

    #[test]
    fn parse_relative_ms_rejects_malformed() {
        for bad in ["", "d", "30", "3.5d", "30x", "-5d", "abc", "30dd"] {
            assert!(
                parse_relative_ms(bad).is_err(),
                "expected error for {bad:?}"
            );
        }
    }

    // --- store-side used aggregation -------------------------------------

    #[test]
    fn collect_used_extracts_mcp_and_skill_calls_and_skips_the_rest() {
        // Arrange: a session with an MCP call, a Skill call, a Skill call with no
        // skill field (the post/other side), a malformed mcp name, and a plain
        // tool. Only the MCP and the well-formed Skill call should be collected.
        let tmp = TempDir::new().unwrap();
        let store = SessionStore::new(tmp.path());
        let mut w = store.open("sess", NOW).unwrap();
        w.append(&tool_call(
            NOW,
            json!({"tool_name": "mcp__cmux__list_agents", "tool_use_id": "a"}),
        ))
        .unwrap();
        w.append(&tool_call(
            NOW + 1,
            json!({"tool_name": "Skill", "tool_input": {"skill": "review"}}),
        ))
        .unwrap();
        w.append(&tool_call(
            NOW + 2,
            json!({"tool_name": "Skill", "tool_input": {}}),
        ))
        .unwrap();
        w.append(&tool_call(NOW + 3, json!({"tool_name": "mcp__"})))
            .unwrap();
        w.append(&tool_call(
            NOW + 4,
            json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
        ))
        .unwrap();
        drop(w);

        // Act
        let (mcp, skills) = collect_used(&store).unwrap();

        // Assert
        assert_eq!(mcp.len(), 1);
        assert_eq!(mcp[0].name, "cmux");
        assert_eq!(mcp[0].ts_ms, NOW);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "review");
    }

    // --- config readers (temp files/dirs) --------------------------------

    #[test]
    fn claude_json_yields_server_keys_and_never_retains_values() {
        // Arrange: a ~/.claude.json shape with a secret env value.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = tmp.path().join("proj");
        fs::create_dir_all(&cwd).unwrap();
        let claude_json = home.join(CLAUDE_JSON);
        let content = json!({
            "mcpServers": {
                "global-srv": {"command": "x", "env": {"TOKEN": "SUPERSECRET"}}
            },
            "projects": {
                cwd.to_string_lossy(): {
                    "mcpServers": {
                        "proj-srv": {"command": "y", "env": {"KEY": "ANOTHERSECRET"}}
                    }
                }
            }
        });
        fs::write(&claude_json, serde_json::to_string(&content).unwrap()).unwrap();

        // Act
        let sources = read_configured_mcp(home, &cwd);

        // Assert: correct keys, and the secret never appears in the source data.
        let global = &sources[0];
        let project = &sources[1];
        assert_eq!(global.names, vec!["global-srv"]);
        assert_eq!(project.names, vec!["proj-srv"]);
        let serialized = serde_json::to_string(&sources).unwrap();
        assert!(!serialized.contains("SUPERSECRET"));
        assert!(!serialized.contains("ANOTHERSECRET"));
        assert!(!serialized.contains("TOKEN"));
        assert!(!serialized.contains("command"));
    }

    #[test]
    fn mcp_json_present_and_missing_produce_read_and_missing() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = tmp.path().join("proj");
        fs::create_dir_all(&cwd).unwrap();

        // Missing .mcp.json → Missing status.
        let sources = read_configured_mcp(home, &cwd);
        assert!(matches!(sources[2].status, SourceStatus::Missing));

        // Present .mcp.json → Read with its keys.
        fs::write(
            cwd.join(MCP_JSON),
            serde_json::to_string(&json!({"mcpServers": {"proj-file-srv": {"command": "z"}}}))
                .unwrap(),
        )
        .unwrap();
        let sources = read_configured_mcp(home, &cwd);
        assert_eq!(sources[2].names, vec!["proj-file-srv"]);
        assert!(matches!(sources[2].status, SourceStatus::Read { count: 1 }));
    }

    #[test]
    fn mcp_json_missing_unreadable_and_parse_failed_are_distinct() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nope.json");
        assert!(matches!(
            read_mcp_json(&missing).status,
            SourceStatus::Missing
        ));

        // A directory where a file is expected → read fails (not NotFound).
        let dir_path = tmp.path().join("adir.json");
        fs::create_dir(&dir_path).unwrap();
        assert!(matches!(
            read_mcp_json(&dir_path).status,
            SourceStatus::Unreadable { .. }
        ));

        // Invalid JSON → ParseFailed.
        let bad = tmp.path().join("bad.json");
        fs::write(&bad, b"{ not valid json").unwrap();
        assert!(matches!(
            read_mcp_json(&bad).status,
            SourceStatus::ParseFailed { .. }
        ));
    }

    #[test]
    fn claude_json_missing_makes_both_derived_sources_missing() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().join("proj");
        fs::create_dir_all(&cwd).unwrap();
        // No ~/.claude.json written.
        let sources = read_configured_mcp(tmp.path(), &cwd);
        assert!(matches!(sources[0].status, SourceStatus::Missing));
        assert!(matches!(sources[1].status, SourceStatus::Missing));
    }

    #[test]
    fn claude_json_parse_failure_marks_both_derived_sources_parse_failed() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().join("proj");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(tmp.path().join(CLAUDE_JSON), b"{ broken").unwrap();
        let sources = read_configured_mcp(tmp.path(), &cwd);
        assert!(matches!(
            sources[0].status,
            SourceStatus::ParseFailed { .. }
        ));
        assert!(matches!(
            sources[1].status,
            SourceStatus::ParseFailed { .. }
        ));
    }

    #[test]
    fn skills_dir_counts_only_subdirs_with_skill_md() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let cwd = tmp.path().join("proj");
        // User skills: one valid, one without SKILL.md.
        let skills = home.join(CLAUDE_DIR).join(SKILLS_DIR);
        fs::create_dir_all(skills.join("witness")).unwrap();
        fs::write(skills.join("witness").join(SKILL_FILE), b"# skill").unwrap();
        fs::create_dir_all(skills.join("empty-dir")).unwrap();
        fs::create_dir_all(&cwd).unwrap();

        let sources = read_configured_skills(home, &cwd);
        assert_eq!(sources[0].source, ConfiguredSource::SkillsUser);
        assert_eq!(sources[0].names, vec!["witness"]);
        // Project skills dir absent → Missing.
        assert!(matches!(sources[1].status, SourceStatus::Missing));
    }

    #[test]
    fn skills_root_that_is_a_file_is_unreadable_not_empty() {
        let tmp = TempDir::new().unwrap();
        // Place a FILE where the skills directory is expected.
        let claude = tmp.path().join(CLAUDE_DIR);
        fs::create_dir_all(&claude).unwrap();
        fs::write(claude.join(SKILLS_DIR), b"not a dir").unwrap();
        let source = read_skills_source(ConfiguredSource::SkillsUser, &claude.join(SKILLS_DIR));
        assert!(matches!(source.status, SourceStatus::Unreadable { .. }));
    }

    // --- rendering --------------------------------------------------------

    #[test]
    fn markdown_uses_factual_labels_and_avoids_bare_prune_wording() {
        let mcp = vec![read_source_of(
            ConfiguredSource::McpUserGlobal,
            &["idle-srv"],
        )];
        let report = build_inventory(mcp, vec![], vec![], vec![], NOW, None);
        let md = to_markdown(&report);
        assert!(md.contains("Configured now, not observed used"));
        assert!(md.contains("Observed used in all recorded history, not configured now"));
        // Neutral header — never a bare "unused".
        assert!(!md.contains("### Unused"));
        // The one advisory line is allowed and clearly labelled.
        assert!(md.contains("Review for pruning: idle-srv"));
        // Scope note honesty markers.
        assert!(md.contains("never attributed as an agent action"));
        assert!(md.contains("never read"));
        assert!(md.contains("server names"));
    }

    #[test]
    fn json_round_trips_and_omits_secret_values() {
        let mcp = vec![read_source_of(ConfiguredSource::McpUserGlobal, &["srv"])];
        let used = vec![obs("srv", NOW)];
        let report = build_inventory(mcp, vec![], used, vec![], NOW, Some(NOW - DAY_MS));
        let js = to_json(&report).unwrap();
        let parsed: Value = serde_json::from_str(&js).unwrap();
        assert_eq!(parsed["now_ms"], json!(NOW));
        assert_eq!(parsed["completeness"], json!("complete"));
        assert_eq!(parsed["mcp"]["used"][0]["name"], json!("srv"));
        assert_eq!(parsed["mcp"]["used"][0]["call_count"], json!(1));
    }

    #[test]
    fn partial_completeness_adds_a_scope_caveat_line() {
        let mcp = vec![SourceRead {
            source: ConfiguredSource::McpProjectFile,
            status: SourceStatus::ParseFailed {
                reason: "x".to_string(),
            },
            names: Vec::new(),
        }];
        let report = build_inventory(mcp, vec![], vec![], vec![], NOW, None);
        let md = to_markdown(&report);
        assert!(md.contains("Completeness: partial"));
        assert!(md.contains("may be incomplete"));
    }
}
