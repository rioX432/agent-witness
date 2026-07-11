//! Non-panicking terminal setup for the TUI commands (`show`, `top`, `--pick`).
//!
//! The `require_tty` pre-check in `main.rs` (`stdout().is_terminal()`) catches
//! the common non-interactive case (pipes, CI). It is necessary but not
//! sufficient: environments exist where stdout reports as a TTY yet ratatui's
//! terminal init still fails with ENXIO ("Device not configured") — e.g. Claude
//! Code's `!` command runner and some embedded/remote shells. The panicking
//! `ratatui::init()` would then panic and leak the `[?1049l` alternate-screen
//! escape, corrupting the terminal — the exact symptom issue #40 set out to
//! prevent. This helper uses the non-panicking `ratatui::try_init()` and, on
//! failure, restores the terminal and returns an actionable error instead
//! (issue #54, defense-in-depth alongside `require_tty`).

use anyhow::{anyhow, Result};
use ratatui::DefaultTerminal;

/// Initialize the alternate-screen terminal for a TUI command, or return an
/// actionable error instead of panicking when init fails.
///
/// `command` is the subcommand name (e.g. `"show"`) and `fallback` is the
/// non-interactive alternative to suggest (e.g. `"agent-witness report"`),
/// mirroring the wording of `require_tty`. On init failure the terminal is
/// restored defensively so no raw-mode / alternate-screen escape leaks, and the
/// returned `Err` propagates to `main`, which prints it and exits non-zero.
pub fn init(command: &str, fallback: &str) -> Result<DefaultTerminal> {
    match ratatui::try_init() {
        Ok(terminal) => Ok(terminal),
        Err(source) => {
            // `try_init` enables raw mode and enters the alternate screen before
            // building the terminal; an early failure can leave raw mode on.
            // Restore defensively (best-effort, ignores errors) so no escape
            // leaks even though we never entered the draw loop.
            ratatui::restore();
            Err(anyhow!(init_failure_message(command, fallback, &source)))
        }
    }
}

/// Format the actionable one-line error for a terminal-init failure. Kept as a
/// pure function so the message wording is unit-testable without a real
/// terminal. Mirrors `require_tty` so both failure paths read the same, and
/// names the underlying cause (e.g. the ENXIO source) for diagnosis.
fn init_failure_message(command: &str, fallback: &str, source: &std::io::Error) -> String {
    format!(
        "`agent-witness {command}` needs an interactive terminal (terminal init failed: {source}). \
         For non-interactive output, use `{fallback}`."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Error;

    #[test]
    fn init_failure_message_is_actionable_and_names_the_cause() {
        // Mimic the observed ENXIO failure ("Device not configured").
        let source = Error::other("Device not configured");
        let message = init_failure_message("show", "agent-witness report", &source);

        assert!(
            message.contains("needs an interactive terminal"),
            "keeps the shared actionable phrasing: {message}"
        );
        assert!(
            message.contains("agent-witness show"),
            "names the command that failed: {message}"
        );
        assert!(
            message.contains("terminal init failed: Device not configured"),
            "surfaces the underlying init error: {message}"
        );
        assert!(
            message.contains("agent-witness report"),
            "suggests the non-interactive fallback: {message}"
        );
        assert!(
            !message.contains("panicked"),
            "an error message, not a panic: {message}"
        );
    }
}
