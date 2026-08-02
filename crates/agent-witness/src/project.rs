//! Project label derived from a session's working directory.
//!
//! Claude Code worktrees live at `<repo>/.claude/worktrees/<worktree-id>[/sub]`,
//! and the worktree id is opaque. Labelling such a cwd by its basename would
//! name the session after an id that identifies no project, so worktree paths
//! are labelled by the repository directory ahead of the marker, plus any
//! sub-path below the worktree id (`viral-loc-monorepo/apps/nine`).

/// Path segment marking a Claude Code worktree root.
const WORKTREE_MARKER: &str = "/.claude/worktrees/";
/// Path separator. Recorded cwds are POSIX paths (hook payloads, macOS/Linux).
const SEP: char = '/';

/// Human-facing project label for a recorded `cwd`.
///
/// Plain paths keep their basename; worktree paths resolve to the repository
/// that owns the worktree.
pub fn project_label(cwd: &str) -> String {
    let path = cwd.trim_end_matches(SEP);
    if let Some((base, rest)) = path.split_once(WORKTREE_MARKER) {
        if let Some(repo) = base.rsplit(SEP).next().filter(|r| !r.is_empty()) {
            // Skip the worktree id itself; anything below it locates the work.
            let tail: Vec<&str> = rest.split(SEP).skip(1).filter(|s| !s.is_empty()).collect();
            return if tail.is_empty() {
                repo.to_string()
            } else {
                format!("{repo}/{}", tail.join("/"))
            };
        }
    }
    basename(path)
}

/// The last path component of `path`, or the path itself if it has none.
fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_path_uses_its_basename() {
        assert_eq!(
            project_label("/Users/rio/workspace/projects/kakimato"),
            "kakimato"
        );
    }

    #[test]
    fn worktree_path_uses_the_owning_repo() {
        assert_eq!(
            project_label(
                "/Users/rio/workspace/projects/kakimato/.claude/worktrees/agent-a1441aa58313b993f"
            ),
            "kakimato"
        );
    }

    #[test]
    fn worktree_subpath_is_appended_after_the_repo() {
        assert_eq!(
            project_label(
                "/Users/rio/workspace/projects/viral-loc-monorepo/.claude/worktrees/agent-a486884176105b0c0/apps/nine"
            ),
            "viral-loc-monorepo/apps/nine"
        );
    }

    #[test]
    fn trailing_slashes_are_ignored() {
        assert_eq!(project_label("/w/proj/"), "proj");
        assert_eq!(
            project_label("/w/proj/.claude/worktrees/agent-1/sub/"),
            "proj/sub"
        );
    }

    #[test]
    fn worktree_marker_without_a_repo_falls_back_to_the_basename() {
        assert_eq!(project_label("/.claude/worktrees/agent-1"), "agent-1");
    }

    #[test]
    fn root_and_empty_paths_yield_no_label() {
        assert_eq!(project_label("/"), "");
        assert_eq!(project_label(""), "");
    }
}
