//! Default runtime paths for the socket and the session store.
//!
//! Paths are resolved here and passed explicitly into [`crate::emit`] and
//! [`crate::watch`] so those functions stay injectable (tests supply tempdir
//! paths and never touch the real home directory).

use std::path::PathBuf;

use anyhow::{anyhow, Result};

/// Application directory / socket sub-namespace name.
const APP_NAME: &str = "agent-witness";
/// Unix socket file name under the runtime directory.
const SOCKET_FILE: &str = "witness.sock";
/// Session store sub-directory under the app home.
const SESSIONS_DIR: &str = "sessions";

/// Resolved runtime paths.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Unix socket the `emit` bridge connects to and `watch` binds.
    pub socket: PathBuf,
    /// Root directory of the JSONL session store.
    pub sessions_root: PathBuf,
}

/// Resolve default paths.
///
/// - Socket: `$XDG_RUNTIME_DIR/agent-witness/witness.sock` when
///   `XDG_RUNTIME_DIR` is set (the correct place for per-user runtime sockets),
///   otherwise `~/.agent-witness/witness.sock`.
/// - Session store: `~/.agent-witness/sessions`.
pub fn resolve() -> Result<Paths> {
    let app_home = home_dir()?.join(format!(".{APP_NAME}"));
    let sessions_root = app_home.join(SESSIONS_DIR);

    let socket_dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join(APP_NAME),
        _ => app_home,
    };

    Ok(Paths {
        socket: socket_dir.join(SOCKET_FILE),
        sessions_root,
    })
}

/// The current user's home directory from `$HOME`. v0.1 targets unix; Windows is
/// best-effort only (see CLAUDE.md), so we do not consult `%USERPROFILE%` here.
pub(crate) fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("cannot determine home directory: $HOME is not set"))
}
