#!/usr/bin/env bash
# capture.sh — record a real Claude Code session's hook payloads to a JSONL dump.
#
# It temporarily points the hooks (SessionStart / UserPromptSubmit / PreToolUse /
# PostToolUse / Stop / SessionEnd) at a file-append command, so each hook's stdin JSON is
# written verbatim — one compact JSON per line. The dump is raw and UNSANITIZED;
# feed it to sanitize.py before committing anything under tests/fixtures/.
#
# Usage:
#   tools/fixtures/capture.sh <dump-file> <work-dir> <prompt> [max-turns]
#
# Example (basic scenario):
#   tools/fixtures/capture.sh /tmp/dump-basic.jsonl /tmp/proj-basic \
#     "Create hello.rs printing Hello, then run 'rustc --version'. Keep it minimal." 8
#
# Requires: `claude` CLI and `jq` on PATH.
set -euo pipefail

if [[ $# -lt 3 ]]; then
  echo "usage: $0 <dump-file> <work-dir> <prompt> [max-turns]" >&2
  exit 2
fi

dump="$1"; workdir="$2"; prompt="$3"; max_turns="${4:-8}"

command -v claude >/dev/null || { echo "error: claude CLI not found" >&2; exit 1; }
command -v jq >/dev/null || { echo "error: jq not found" >&2; exit 1; }

# The dump path is embedded single-quoted in the hook command below, and hooks
# run with cwd=<workdir> — so it must be absolute and must not contain a quote.
case "$dump" in
  /*) : ;;
  *) dump="$PWD/$dump" ;;
esac
if [[ "$dump" == *"'"* ]]; then
  echo "error: dump path must not contain a single quote: $dump" >&2
  exit 2
fi

mkdir -p "$workdir"
: > "$dump"

# Build a settings file whose hooks append each stdin payload to the dump.
settings="$(mktemp)"
trap 'rm -f "$settings"' EXIT
append_cmd="jq -c . >> '$dump'"
jq -n --arg cmd "$append_cmd" '
  def with_matcher: [{matcher:"*", hooks:[{type:"command", command:$cmd}]}];
  def no_matcher:  [{hooks:[{type:"command", command:$cmd}]}];
  {hooks:{
     SessionStart:     no_matcher,
     UserPromptSubmit: no_matcher,
     PreToolUse:       with_matcher,
     PostToolUse:      with_matcher,
     Stop:             no_matcher,
     SessionEnd:       no_matcher
  }}' > "$settings"

# `--settings` ADDS to (does not replace) the user's global settings, so any
# local hooks (e.g. command proxies) may also fire; sanitize.py normalizes those.
( cd "$workdir" && claude -p "$prompt" \
    --settings "$settings" \
    --max-turns "$max_turns" \
    --dangerously-skip-permissions >/dev/null )

echo "captured $(wc -l < "$dump" | tr -d ' ') hook events -> $dump" >&2
