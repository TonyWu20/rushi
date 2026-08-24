#!/usr/bin/env bash
set -euo pipefail

SESSION="$1"
CONFIG="${CONFIG:-config.toml}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MAX_STEPS=$(awk -F'[[:space:]]*=[[:space:]]*' '/^max_steps[[:space:]]*=/{print $2; exit}' "$CONFIG")
MAX_STEPS=${MAX_STEPS:-20}
SESSIONS_ROOT=$(awk -F'"' '/^sessions_root[[:space:]]*=/{print $2; exit}' "$CONFIG")
STEPS=0

while [ "$STEPS" -lt "$MAX_STEPS" ]; do
  STEPS=$((STEPS + 1))
  "$SCRIPT_DIR/step.sh" "$SESSION" || exit 1
  BIN_DIR="$(cd "$SCRIPT_DIR/../target/debug" && pwd)"
  STATE=$("$BIN_DIR/claim" --session "$SESSIONS_ROOT/$SESSION" | jq -r .state)
  if [ "$STATE" = "idle" ]; then
    exit 0
  fi
done

echo "max_steps reached ($MAX_STEPS)" >&2
exit 1
