//! `agent-witness` CLI entry point.
//!
//! v0.1 subcommands landed here: `emit` (hooks bridge) and `watch` (socket
//! server). Other commands (init/ls/show/report/TUI) arrive in later issues.

use std::sync::Arc;

use agent_witness::{emit, paths, watch};
use agent_witness_core::{Clock, SystemClock};
use anyhow::Result;
use clap::{Parser, Subcommand};
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
    /// Claude Code hooks command: read one hook payload on stdin and record it
    /// (forwards to the `watch` daemon, or writes to the store if none is up).
    Emit,
    /// Run the unix socket server: receive hook payloads, normalize, and store.
    Watch,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = paths::resolve()?;

    match cli.command {
        Command::Emit => {
            let mut payload = String::new();
            tokio::io::stdin().read_to_string(&mut payload).await?;
            emit::run_emit(&paths.socket, &paths.sessions_root, payload).await?;
        }
        Command::Watch => {
            let clock: Arc<dyn Clock + Send + Sync> = Arc::new(SystemClock);
            watch::run_watch(&paths.socket, &paths.sessions_root, clock, async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        }
    }

    Ok(())
}
