//! `agent-witness` CLI entry point.
//!
//! v0.1 subcommands landed here: `init` (hooks registration), `emit` (hooks
//! bridge), and `watch` (socket server). Other commands (ls/show/report/TUI)
//! arrive in later issues.

use std::sync::Arc;

use agent_witness::init::{self, InitOutcome, InitReport};
use agent_witness::report::{self, JsonExporter, MarkdownExporter, SessionExporter};
use agent_witness::{emit, ls, paths, pick, tui, watch};
use agent_witness_core::{collect_summaries, resolve, Clock, SessionStore, SystemClock};
use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use tokio::io::AsyncReadExt;

/// Session recorder and audit log for AI coding agents.
#[derive(Debug, Parser)]
#[command(name = "agent-witness", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Register the Claude Code hooks that record this session (or remove them
    /// with --remove). Edits settings.json safely: idempotent, merge-preserving,
    /// and backed up before any change.
    Init(InitArgs),
    /// Claude Code hooks command: read one hook payload on stdin and record it
    /// (forwards to the `watch` daemon, or writes to the store if none is up).
    Emit(TranscriptArgs),
    /// Run the unix socket server: receive hook payloads, normalize, and store.
    Watch(TranscriptArgs),
    /// List recorded sessions (start time, event/tool counts, duration).
    Ls,
    /// Open the interactive timeline viewer for a recorded session.
    Show(ShowArgs),
    /// Print a shareable markdown (or --json) audit report for a session.
    Report(ReportArgs),
}

/// Arguments for `agent-witness report`.
#[derive(Debug, Args)]
struct ReportArgs {
    /// Session selector (id prefix, `@last`, `@2`, `@project:<sub>`,
    /// `@live:<n>`). Omit to report on the latest session for this directory.
    session: Option<String>,
    /// Emit machine-readable JSON instead of markdown (same data).
    #[arg(long)]
    json: bool,
}

/// Arguments for `agent-witness show`.
#[derive(Debug, Args)]
struct ShowArgs {
    /// Session selector (id prefix, `@last`, `@2`, `@project:<sub>`,
    /// `@live:<n>`). Omit to view the latest session for this directory.
    session: Option<String>,
    /// Start from an interactive session list and drill down into the one you
    /// pick (merges the `ls` → `show` two-step into one command).
    #[arg(long)]
    pick: bool,
}

/// Arguments for `agent-witness init`.
#[derive(Debug, Args)]
struct InitArgs {
    /// Edit ./.claude/settings.json instead of ~/.claude/settings.json.
    #[arg(long)]
    project: bool,
    /// Remove agent-witness hook entries instead of adding them.
    #[arg(long)]
    remove: bool,
}

/// Flags shared by the recording commands controlling the transcript adapter.
#[derive(Debug, Args)]
struct TranscriptArgs {
    /// Disable the best-effort transcript adapter (default: enabled). When off,
    /// recording is hooks-only — the canonical source is unaffected.
    #[arg(long)]
    no_transcript: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Init(args) => {
            let path = init::resolve_settings_path(args.project)?;
            let report = init::run_init(&path, args.remove, &SystemClock)?;
            report_init(&path.display().to_string(), &report);
        }
        Command::Emit(args) => {
            let paths = paths::resolve()?;
            let mut payload = String::new();
            tokio::io::stdin().read_to_string(&mut payload).await?;
            emit::run_emit(
                &paths.socket,
                &paths.sessions_root,
                payload,
                !args.no_transcript,
            )
            .await?;
        }
        Command::Watch(args) => {
            let paths = paths::resolve()?;
            let clock: Arc<dyn Clock + Send + Sync> = Arc::new(SystemClock);
            watch::run_watch(
                &paths.socket,
                &paths.sessions_root,
                clock,
                !args.no_transcript,
                async {
                    let _ = tokio::signal::ctrl_c().await;
                },
            )
            .await?;
        }
        Command::Ls => {
            let paths = paths::resolve()?;
            let store = SessionStore::new(&paths.sessions_root);
            let rows = ls::collect_rows(&store)?;
            print!("{}", ls::render_table(&rows));
        }
        Command::Show(args) => {
            let paths = paths::resolve()?;
            let store = SessionStore::new(&paths.sessions_root);
            // Resolve which session to open before touching the terminal, so
            // selector errors and the cwd-fallback note print as plain text.
            let session = if args.pick {
                let summaries = collect_summaries(&store)?;
                let rows = pick::rows_from_summaries(&summaries, SystemClock.now_ms());
                // The picker and viewer both drive blocking crossterm loops; keep
                // them off the async reactor so polling never starves the runtime.
                tokio::task::spawn_blocking(move || pick::run_pick(rows)).await??
            } else {
                Some(resolve_session(&store, args.session.as_deref())?)
            };
            let Some(session) = session else {
                return Ok(()); // Picker was dismissed without a choice.
            };
            tokio::task::spawn_blocking(move || tui::run_show(&store, &session)).await??;
        }
        Command::Report(args) => {
            let paths = paths::resolve()?;
            let store = SessionStore::new(&paths.sessions_root);
            let session = resolve_session(&store, args.session.as_deref())?;
            let read = store.read(&session)?;
            // Prefer the recorded session start; fall back to the first event's
            // time. All times derive from event data — no wall-clock (determinism).
            let started_ms = store
                .read_meta(&session)
                .ok()
                .map(|m| m.created_ts)
                .or_else(|| read.events.first().map(|e| e.ts));
            let report = report::build_report(&session, &read, started_ms);
            let rendered = if args.json {
                JsonExporter.export(&report)?
            } else {
                MarkdownExporter.export(&report)?
            };
            print!("{rendered}");
        }
    }

    Ok(())
}

/// Resolve a session `selector` (or `None` for the no-argument default) to a
/// concrete session id, printing a one-line note to stderr when the no-arg
/// default falls back from the current directory to the globally latest session.
///
/// stderr keeps the note off stdout so `report`/`report --json` output stays
/// pipeable. "Now" comes from the system clock; core resolution stays pure.
fn resolve_session(store: &SessionStore, selector: Option<&str>) -> Result<String> {
    let summaries = collect_summaries(store)?;
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let resolution = resolve(&summaries, selector, &cwd, SystemClock.now_ms())?;
    if resolution.cwd_fallback {
        eprintln!(
            "No session recorded for {cwd}; showing the most recent session ({}) instead.",
            resolution.session_id
        );
    }
    Ok(resolution.session_id)
}

/// Print a short, honest summary of what `init` did.
fn report_init(path: &str, report: &InitReport) {
    match report.outcome {
        InitOutcome::Installed => println!("Registered agent-witness hooks in {path}"),
        InitOutcome::AlreadyInstalled => {
            println!("agent-witness hooks already registered in {path}; no changes")
        }
        InitOutcome::Removed => println!("Removed agent-witness hooks from {path}"),
        InitOutcome::NothingToRemove => {
            println!("No agent-witness hooks found in {path}; nothing to remove")
        }
    }
    if let Some(backup) = &report.backup {
        println!("Backed up previous settings to {}", backup.display());
    }
}
