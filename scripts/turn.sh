#!/usr/bin/env bash
set -euo pipefail

# No step cap. The loop runs until claim reports idle.
# See entry 51 in docs/loop-and-edit-implementation-corrections.md.

SESSION="$1"
CONFIG="${CONFIG:-config.toml}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN_DIR="$(cd "$SCRIPT_DIR/../target/debug" && pwd)"
SESSIONS_ROOT=$(awk -F'"' '/^sessions_root[[:space:]]*=[[:space:]]*/{print $2; exit}' "$CONFIG")

while true; do
  "$SCRIPT_DIR/step.sh" "$SESSION" || exit 1
  STATE=$("$BIN_DIR/claim" --session "$SESSIONS_ROOT/$SESSION" | jq -r .state)
  if [ "$STATE" = "idle" ]; then
    break
  fi
  # exhausted: the automatic handoff recorded a context_exhausted
  # marker and seeded the next session (correction 57). The TUI
  # offers the one-key resume in the seeded session.
  if [ "$STATE" = "exhausted" ]; then
    break
  fi
done

# Print a readable transcript of the current turn.
jq -c -s -f "$SCRIPT_DIR/transcript.jq" "$SESSIONS_ROOT/$SESSION/events.jsonl" 2>/dev/null
exit 0
