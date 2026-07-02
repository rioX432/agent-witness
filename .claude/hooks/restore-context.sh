#!/usr/bin/env bash
# Restore important context after compaction or at session start
# Called as PostCompact / SessionStart hook
#
# Injects critical information that should survive context compression.
# Outputs standard hook JSON via jq for proper escaping.

set -euo pipefail

# Check for progress.txt (used during long tasks)
if [ -f "progress.txt" ]; then
  PROGRESS=$(head -20 progress.txt)
  jq -n --arg msg "Context restored. Current progress:
$PROGRESS" '{"additionalContext": $msg}'
  exit 0
fi

# No special context to restore
exit 0
