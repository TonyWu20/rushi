#!/usr/bin/env bash
set -uo pipefail

SESSION="$1"
CONFIG="${CONFIG:-config.toml}"
SESSIONS_ROOT=$(awk -F'"' '/^sessions_root[[:space:]]*=/{print $2; exit}' "$CONFIG")
SESSION_DIR="$SESSIONS_ROOT/$SESSION"
WORKDIR=$(mktemp -d "${TMPDIR:-/tmp}/step.XXXXXX")
trap 'rm -rf "$WORKDIR"' EXIT

# 1. claim: pure projection. Decide what is owed.
BIN_DIR="$(cd "$(dirname "$0")/../target/debug" && pwd)"
TOOL_DIR="$(cd "$(dirname "$0")/../tools" && pwd)"
SCHEMA_DIR="$(cd "$(dirname "$0")/../schemas/events/v1" && pwd)"

"$BIN_DIR/claim" --session "$SESSION_DIR" > "$WORKDIR/claim.json" || exit 1
STATE=$(jq -r .state "$WORKDIR/claim.json")

# 2. idle: nothing owed. Append nothing (G1 idempotent replay).
if [ "$STATE" = "idle" ]; then
  exit 0
fi

# 3. awaiting_tool_result: crash recovery (G2).
#    Route the pending calls without calling the model.
if [ "$STATE" = "awaiting_tool_result" ]; then
  TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  jq -c '.pending_tool_calls[] | . + {type: "tool_call", ts: $ts}' --arg ts "$TS" "$WORKDIR/claim.json" \
    | "$BIN_DIR/route" --tools "$TOOL_DIR" > "$WORKDIR/routed.jsonl" || exit 1
  "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/routed.jsonl" || exit 1
  exit 0
fi

# 4. awaiting_model: assemble, then check for a budget error before model.
"$BIN_DIR/assemble" --session "$SESSION_DIR" --config "$CONFIG" > "$WORKDIR/model-request.json" || exit 1
if jq -e '.type == "error"' "$WORKDIR/model-request.json" > /dev/null; then
  "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/model-request.json" || exit 1
  exit 0
fi

# 5. model: one API call. Capture exit code.
set +e
"$BIN_DIR/model" --config "$CONFIG" < "$WORKDIR/model-request.json" > "$WORKDIR/model-output.json"
MODEL_EXIT=$?
set -e

if [ "$MODEL_EXIT" -ne 0 ]; then
  # Model failed. Log an error event and stop.
  ERROR_EVENT=$(jq -cn --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{v:1, type:"error", ts:$ts, message:"model API call failed"}')
  echo "$ERROR_EVENT" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
  exit 0
fi

# 6. parse: validate, emit events, choose exit code.
set +e
"$BIN_DIR/parse" --config "$CONFIG" < "$WORKDIR/model-output.json" > "$WORKDIR/parsed.jsonl"
PARSE_EXIT=$?
set -e

# 7. route only when parse says tool calls need routing (exit 1).
if [ "$PARSE_EXIT" -eq 1 ]; then
  jq -c 'select(.type == "tool_call")' "$WORKDIR/parsed.jsonl" \
    | "$BIN_DIR/route" --tools "$TOOL_DIR" > "$WORKDIR/routed.jsonl" || exit 1
  cat "$WORKDIR/parsed.jsonl" "$WORKDIR/routed.jsonl" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
else
  "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/parsed.jsonl" || exit 1
fi

exit 0
