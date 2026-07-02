//! `agent-witness` CLI entry point.
//!
//! v0.1 subcommands landed here: `init` (hooks registration), `emit` (hooks
//! bridge), and `watch` (socket server). Other commands (ls/show/report/TUI)
//! arrive in later issues.

use std::sync::Arc;

use agent_witness::init::{self, InitOutcome, InitReport};
use agent_witness::{emit, paths, watch};
use agent_witness_core::{Clock, SystemClock};
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
    }

    Ok(())
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
