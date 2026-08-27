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

# The session working directory, recorded at the entry point.
TOOL_CWD=""
if [ -f "$SESSION_DIR/cwd" ]; then
  TOOL_CWD=$(cat "$SESSION_DIR/cwd")
fi

route_cmd() {
  if [ -n "$TOOL_CWD" ]; then
    "$BIN_DIR/route" --tools "$TOOL_DIR" --cwd "$TOOL_CWD" "$@"
  else
    "$BIN_DIR/route" --tools "$TOOL_DIR" "$@"
  fi
}

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
    | route_cmd > "$WORKDIR/routed.jsonl" || exit 1
  "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/routed.jsonl" || exit 1
  exit 0
fi

# 4. awaiting_model: assemble, then check for a budget error before model.
"$BIN_DIR/assemble" --session "$SESSION_DIR" --config "$CONFIG" > "$WORKDIR/model-request.json" || exit 1
if jq -e '.type == "error"' "$WORKDIR/model-request.json" > /dev/null; then
  "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/model-request.json" || exit 1
  exit 0
fi

# 5-6. model + parse, with a guard against empty assistant turns.
# An empty turn (no text and no tool calls) is a model glitch.
# Retry up to EMPTY_RETRIES times. If it persists, log an error and stop.
# A failed model call (API or stream error) is a transport problem. Retry
# up to MODEL_ERR_RETRIES times before logging the error and stopping.
EMPTY_RETRIES=3
MODEL_ERR_RETRIES=2
ATTEMPT=0
MODEL_ERR_ATTEMPT=0
while true; do
  ATTEMPT=$((ATTEMPT + 1))

  # model: one API call. Capture exit code.
  set +e
  "$BIN_DIR/model" --config "$CONFIG" < "$WORKDIR/model-request.json" > "$WORKDIR/model-output.json"
  MODEL_EXIT=$?
  set -e

  if [ "$MODEL_EXIT" -ne 0 ]; then
    # The model binary crashed. Treat it as a transient failure.
    if [ "$MODEL_ERR_ATTEMPT" -lt "$MODEL_ERR_RETRIES" ]; then
      MODEL_ERR_ATTEMPT=$((MODEL_ERR_ATTEMPT + 1))
      sleep 3
      continue
    fi
    ERROR_EVENT=$(jq -cn --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      '{v:1, type:"error", ts:$ts, message:"model binary failed to run"}')
    echo "$ERROR_EVENT" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
    exit 0
  fi

  # Transport-level model failure (API error, truncated stream). The model
  # binary reports it with stop_reason "error" and a detail. Retry while
  # attempts remain. Then log the detail and stop.
  MR=$(jq -r '.stop_reason // "none"' "$WORKDIR/model-output.json")
  if [ "$MR" = "error" ]; then
    if [ "$MODEL_ERR_ATTEMPT" -lt "$MODEL_ERR_RETRIES" ]; then
      MODEL_ERR_ATTEMPT=$((MODEL_ERR_ATTEMPT + 1))
      sleep 3
      continue
    fi
    DETAIL=$(jq -r '.detail // "no detail"' "$WORKDIR/model-output.json")
    ERROR_EVENT=$(jq -cn --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg d "$DETAIL" \
      '{v:1, type:"error", ts:$ts, message:("model API call failed after retries: " + $d)}')
    echo "$ERROR_EVENT" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
    exit 0
  fi

  # parse: validate, emit events, choose exit code.
  set +e
  "$BIN_DIR/parse" --config "$CONFIG" < "$WORKDIR/model-output.json" > "$WORKDIR/parsed.jsonl"
  PARSE_EXIT=$?
  set -e

  # Detect an empty assistant turn: no text and no tool calls.
  EMPTY=$(jq -c 'select(.type == "assistant_message") | select(((.content // "") | length) == 0) | select(((.tool_calls // []) | length) == 0) | "empty"' "$WORKDIR/parsed.jsonl" | head -1)

  # An output-budget exhaustion is not a glitch. The model hit
  # max_output_tokens mid-generation. Do not burn retries on it.
  if [ -n "$EMPTY" ]; then
    LAST_STOP=$(jq -r 'select(.type == "assistant_message") | .stop_reason // ""' "$WORKDIR/parsed.jsonl" | head -1)
    if [ "$LAST_STOP" = "length" ]; then
      ERROR_EVENT=$(jq -cn --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        '{v:1, type:"error", ts:$ts, message:"model output budget exhausted: no content after max_output_tokens"}')
      echo "$ERROR_EVENT" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
      exit 0
    fi
  fi

  # Empty turn is a glitch. Retry while attempts remain.
  if [ -n "$EMPTY" ] && [ "$ATTEMPT" -lt "$EMPTY_RETRIES" ]; then
    continue
  fi

  # Empty turn persisted. Log an error and stop.
  if [ -n "$EMPTY" ]; then
    ERROR_EVENT=$(jq -cn --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      '{v:1, type:"error", ts:$ts, message:"model returned an empty turn after retries"}')
    echo "$ERROR_EVENT" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
    exit 0
  fi

  break
done

# 7. route only when parse says tool calls need routing (exit 1).
if [ "$PARSE_EXIT" -eq 1 ]; then
  jq -c 'select(.type == "tool_call")' "$WORKDIR/parsed.jsonl" \
    | route_cmd > "$WORKDIR/routed.jsonl" || exit 1
  cat "$WORKDIR/parsed.jsonl" "$WORKDIR/routed.jsonl" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
else
  "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/parsed.jsonl" || exit 1
fi

exit 0
