#!/usr/bin/env bash
# Save critical context before compaction
# Called as PreCompact hook
#
# Captures task progress and active branch info so PostCompact can restore it.
# Exit code 0 = allow compaction (with saved context)
# Exit code 2 = block compaction (not used here — blocking is for special cases)

set -euo pipefail

CONTEXT_FILE="progress.txt"

# Capture current git state
BRANCH=$(git branch --show-current 2>/dev/null || echo "unknown")
CHANGED=$(git diff --name-only 2>/dev/null | head -10 || true)
STAGED=$(git diff --cached --name-only 2>/dev/null | head -10 || true)

# Read existing task list if present
TASKS=""
if command -v claude &>/dev/null; then
  # Tasks are managed by Claude Code internally; just note the branch context
  :
fi

# Build context snapshot
{
  echo "=== Context snapshot (pre-compact) ==="
  echo "Branch: $BRANCH"
  echo "Timestamp: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if [ -n "$CHANGED" ]; then
    echo "Changed files:"
    echo "$CHANGED" | sed 's/^/  /'
  fi
  if [ -n "$STAGED" ]; then
    echo "Staged files:"
    echo "$STAGED" | sed 's/^/  /'
  fi
  # Preserve any existing progress notes
  if [ -f "$CONTEXT_FILE" ]; then
    echo "---"
    echo "Previous progress:"
    head -20 "$CONTEXT_FILE"
  fi
} > "${CONTEXT_FILE}.tmp" && mv "${CONTEXT_FILE}.tmp" "$CONTEXT_FILE"

exit 0
