#!/usr/bin/env bash
# Log subagent and session lifecycle events
# Called by SubagentStart, SubagentStop, TaskCompleted, SessionEnd hooks
#
# Reads JSON from stdin and appends a structured log entry.

set -euo pipefail

INPUT=$(timeout 3 cat || echo '{}')

HOOK_EVENT=$(echo "$INPUT" | jq -r '.hook_event // "unknown"' 2>/dev/null || echo "unknown")
AGENT_NAME=$(echo "$INPUT" | jq -r '.agent_name // .subagent_name // "main"' 2>/dev/null || echo "main")
SESSION_ID=$(echo "$INPUT" | jq -r '.session_id // "unknown"' 2>/dev/null || echo "unknown")
RESULT=$(echo "$INPUT" | jq -c '.result // .summary // null' 2>/dev/null || echo "null")
TIMESTAMP=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

LOG_DIR="logs/subagents"
mkdir -p "$LOG_DIR"

LOG_FILE="$LOG_DIR/$(date +%Y-%m-%d).jsonl"
echo "{\"timestamp\":\"$TIMESTAMP\",\"event\":\"$HOOK_EVENT\",\"agent\":\"$AGENT_NAME\",\"session\":\"$SESSION_ID\",\"result\":$RESULT}" >> "$LOG_FILE"

exit 0
