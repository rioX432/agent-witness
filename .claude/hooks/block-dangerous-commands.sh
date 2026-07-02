#!/bin/bash
# Block dangerous commands from being executed by AI agents.
# Exit code 2 = block the tool use.

INPUT="$1"

# Dangerous patterns
if echo "$INPUT" | grep -qE 'rm -rf|git push --force|git reset --hard|DROP TABLE|DROP DATABASE'; then
  echo "BLOCKED: Dangerous command detected"
  exit 2
fi

exit 0
