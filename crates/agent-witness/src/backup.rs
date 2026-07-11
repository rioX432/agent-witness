//! Timestamped backups for the files `init` manages outside our own store.
//!
//! Both the hooks settings edit ([`crate::init`]) and the skill install
//! ([`crate::skill`]) rewrite files the user may have touched; both preserve
//! the previous content as `<name>.bak-<ts>` next to the original via this one
//! helper, so the backup naming can never drift between the two.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Prefix for the timestamped backup file (`<name>.bak-<ts>`).
const BACKUP_PREFIX: &str = ".bak-";

/// Write `contents` to the backup path for `original` and return that path.
pub(crate) fn write_backup(
    original: &Path,
    contents: impl AsRef<[u8]>,
    ts: i64,
) -> Result<PathBuf> {
    let backup = backup_path(original, ts);
    std::fs::write(&backup, contents)
        .with_context(|| format!("cannot write backup {}", backup.display()))?;
    Ok(backup)
}

/// `<file>` -> `<file>.bak-<ts>` in the same directory.
pub(crate) fn backup_path(original: &Path, ts: i64) -> PathBuf {
    let mut name = original
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(format!("{BACKUP_PREFIX}{ts}"));
    original.with_file_name(name)
}
