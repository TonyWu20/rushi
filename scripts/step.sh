#!/usr/bin/env bash
set -uo pipefail

SESSION="$1"
CONFIG="${CONFIG:-config.toml}"
SESSIONS_ROOT=$(awk -F'"' '/^sessions_root[[:space:]]*=/{print $2; exit}' "$CONFIG")
SESSION_DIR="$SESSIONS_ROOT/$SESSION"
# The assemble arguments, built from this step's own session.
# A caller-exported ASSEMBLE_ARGS must not chain stale session
# arguments into the loop.
ASSEMBLE_ARGS=(--session "$SESSION_DIR" --config "$CONFIG")
WORKDIR=$(mktemp -d "${TMPDIR:-/tmp}/step.XXXXXX")
trap 'rm -rf "$WORKDIR"' EXIT
# A fatal signal skips the EXIT trap. Clean the workdir and exit on
# Ctrl+C (INT) or TERM, so the step leaves no /tmp/step.XXXXXX dir.
trap 'rm -rf "$WORKDIR"; exit 1' INT TERM

# 1. claim: pure projection. Decide what is owed.
BIN_DIR="$(cd "$(dirname "$0")/../target/debug" && pwd)"
TOOL_DIR="$(cd "$(dirname "$0")/../tools" && pwd)"
SCHEMA_DIR="$(cd "$(dirname "$0")/../schemas/events/v1" && pwd)"

# The binary overrides: the e2e suite points the loop at stub
# binaries. The defaults are the workspace build output.
MODEL_BIN="${MODEL_BIN:-$BIN_DIR/model}"
COMPACT_BIN="${COMPACT_BIN:-$BIN_DIR/compact}"
ASSEMBLE_BIN="${ASSEMBLE_BIN:-$BIN_DIR/assemble}"

# The session working directory, recorded at the entry point.
TOOL_CWD=""
if [ -f "$SESSION_DIR/cwd" ]; then
  TOOL_CWD=$(cat "$SESSION_DIR/cwd")
fi

route_cmd() {
  # The per-session tool log takes the full tool output; the event log
  # gets the slim index (docs/tool-log-design_from_human.md).
  if [ -n "$TOOL_CWD" ]; then
    "$BIN_DIR/route" --tools "$TOOL_DIR" --cwd "$TOOL_CWD" \
      --tool-log "$SESSION_DIR/tools.jsonl" "$@"
  else
    "$BIN_DIR/route" --tools "$TOOL_DIR" \
      --tool-log "$SESSION_DIR/tools.jsonl" "$@"
  fi
}

# Publish the loop phase as an ext_status marker
# (docs/tui-model-wait-indicator.md). The TUI renders the last
# `loop_phase` value, gated on the loop-running bit. One event per
# line, validated through bin/log with the schema dir. A failed
# append aborts the step, like every other append in this script.
append_loop_phase() {
  local phase="$1"
  local ts event
  ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  event=$(jq -cn --arg ts "$ts" --arg p "$phase" \
    '{v:1, type:"ext_status", ts:$ts, id:"loop_phase", value:$p}')
  echo "$event" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
}

# Publish the active model's thinking level (docs/tui.md section 7.2,
# docs/ui-extension.md section 5). The TUI colors the input-area
# border from the last `model_thinking` value. The level resolves
# through `bin/model --describe` — the same config resolution as the
# API call — so the published level matches what the model receives.
#
# The publisher sends only on change (docs/ui-extension.md section
# 5): the effort is frozen call config, so the log carries one event
# per value. A describe failure (no binary, broken config) skips the
# publish; the TUI falls back to its default level. A failed append
# aborts the step, like every other append in this script.
publish_model_thinking() {
  local level last ts event
  level=$("$MODEL_BIN" --describe --config "$CONFIG" 2>/dev/null \
    | jq -r '.thinking_level // empty')
  [[ "$level" =~ ^[0-9]+$ ]] || return 0
  # The on-change gate: the last published value in the log. The tail
  # window is whole lines; a value older than the window republishes
  # the same number once and the log quiets again.
  last=$(tail -n 4096 "$SESSION_DIR/events.jsonl" 2>/dev/null \
    | jq -rs '[.[] | select(.type == "ext_status" and .id == "model_thinking") | .value] | last // empty')
  [[ "$last" == "$level" ]] && return 0
  ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  event=$(jq -cn --arg ts "$ts" --argjson v "$level" \
    '{v:1, type:"ext_status", ts:$ts, id:"model_thinking", value:$v}')
  echo "$event" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
}

# The auto-compact recovery knobs, resolved once per step. The
# kill switch (docs/auto-compact-plan.md section 4.5) gates the
# threshold hook and the overflow compaction. Off sends the overflow
# path to the last-resort compaction without the overflow compaction.
# The active model and its window feed the model guard and the
# truncation test; the input budget is the assemble clamp: the model
# window minus the output reservation, capped by the user knob.
resolve_compact_config() {
  local active="${1:-}"
  ACTIVE_MODEL="${1:-}"
  COMPACT_ENABLED=$(awk '
    /^\[/ { s = $0; sub(/^\[/, "", s); sub(/\].*$/, "", s) }
    s == "limits" && /^compact_enabled[[:space:]]*=/ {
      val = $0; sub(/^[^=]*=[[:space:]]*/, "", val); print val; exit
    }' "$CONFIG")
  [[ -n "$COMPACT_ENABLED" ]] || COMPACT_ENABLED=true
  MAX_OUT=$(awk -v m="$active" '
    /^\[/ { s = $0; sub(/^\[/, "", s); sub(/\].*$/, "", s) }
    /^max_output_tokens[[:space:]]*=/ {
      val = $0; sub(/^[^=]*=[[:space:]]*/, "", val)
      if (s == "model" || s == "model.\"" m "\"" || s == "model." m) { last = val }
    }
    END { print last }' "$CONFIG")
  [[ "${MAX_OUT:-0}" =~ ^[0-9]+$ ]] || MAX_OUT=32768
  CTX_TOK=$(awk -v m="$active" '
    /^\[/ { s = $0; sub(/^\[/, "", s); sub(/\].*$/, "", s) }
    /^context_tokens[[:space:]]*=/ {
      val = $0; sub(/^[^=]*=[[:space:]]*/, "", val)
      if (s == "model." m || s == "model.\"" m "\"" ) { last = val }
    }
    END { print last }' "$CONFIG")
  [[ "${CTX_TOK:-0}" =~ ^[0-9]+$ ]] || CTX_TOK=131072
  local budget_knob
  budget_knob=$(awk '
    /^\[/ { s = $0; sub(/^\[/, "", s); sub(/\].*$/, "", s) }
    s == "limits" && /^context_budget_tokens[[:space:]]*=/ {
      val = $0; sub(/^[^=]*=[[:space:]]*/, "", val); print val; exit
    }' "$CONFIG")
  INPUT_BUDGET=$(awk -v bknob="${budget_knob:-0}" -v ctx="$CTX_TOK" -v maxout="$MAX_OUT" 'BEGIN {
    win = ctx - maxout; if (win < 1) win = 1
    b = (bknob + 0 > 0) ? (bknob + 0) : win
    if (b > win) b = win
    if (b < 1) b = 1
    print b
  }')
}

# Append one event line through the log binary, like the other
# appends in this script. A failed append aborts the step.
log_event() {
  local session_dir="$1"; shift
  echo "$@" | "$BIN_DIR/log" --session "$session_dir" --schemas "$SCHEMA_DIR" || exit 1
}

# The last-resort in-session compaction (docs/auto-compact-plan.md
# section 4.4). It runs the forced compact, re-projects through the
# boundary when the compact succeeded, and prints 0 on success, 1
# on failure.
run_last_resort_compaction() {
  local status
  status=$("$COMPACT_BIN" "$SESSION_DIR" --config "$CONFIG" \
    --reason "${1:-overflow}" --force 2>/dev/null | jq -r '.status // "failed"')
  if [[ "$status" == "compacted" ]]; then
    "$ASSEMBLE_BIN" "${ASSEMBLE_ARGS[@]}" > "$WORKDIR/model-request.json" || exit 1
    return 0
  fi
  return 1
}

# The thinking level publishes on every step entry, before the claim:
# the border color reflects the active model even while the step
# routes tools or the session idles. The on-change gate keeps the
# log quiet after the first publish.
publish_model_thinking

"$BIN_DIR/claim" --session "$SESSION_DIR" > "$WORKDIR/claim.json" || exit 1
STATE=$(jq -r .state "$WORKDIR/claim.json")

# 2. idle: nothing owed. Append nothing (G1 idempotent replay).
FOLLOW_UPS=$(jq -r '.pending_follow_ups | length' "$WORKDIR/claim.json")
if [ "$STATE" = "idle" ]; then
  if [ "$FOLLOW_UPS" -eq 0 ]; then
    exit 0
  fi
  # The follow drain (docs/tui-pending-user-messages.md stage 2):
  # the queued follow messages ride this new turn, one turn per
  # drain. The flag is this step's. In-flight steps of the turn
  # see a live state and do not re-inject.
  ASSEMBLE_ARGS+=("--inject-follow")
fi

# 2.5. exhausted: the handoff closed this session (correction 57).
# Nothing is owed here. The seeded session holds the task. The TUI
# resumes there with one key.
if [ "$STATE" = "exhausted" ]; then
  exit 0
fi

# 3. awaiting_tool_result: crash recovery (G2).
#    Route the pending calls without calling the model.
if [ "$STATE" = "awaiting_tool_result" ]; then
  append_loop_phase tools
  TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  jq -c '.pending_tool_calls[] | . + {type: "tool_call", ts: $ts}' --arg ts "$TS" "$WORKDIR/claim.json" \
    | route_cmd > "$WORKDIR/routed.jsonl" || exit 1
  "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/routed.jsonl" || exit 1
  exit 0
fi

# 4. awaiting_model: the threshold compact hook, assemble, the
# Exhausted-form last resort, and the model call with overflow
# recovery (docs/auto-compact-plan.md section 4.4). The marker
# covers assemble, the compact calls, the model call, the parse,
# and the retry loop (docs/tui-model-wait-indicator.md).
append_loop_phase wait

# The classifier table and the config knobs. The active model comes
# from the same resolution as the API call (the model guard's source
# of truth).
source "$(cd "$(dirname "$0")" && pwd)/overflow-classify.sh"
ACTIVE_MODEL=$("$MODEL_BIN" --describe --config "$CONFIG" 2>/dev/null | jq -r '.active // empty')
# The guard source: the request model is the model_id of the active
# model (assemble writes model_id into every request). The section
# name and the model_id differ in the deepseek config.
GUARD_MODEL=$("$MODEL_BIN" --describe --config "$CONFIG" 2>/dev/null | jq -r '.model_id // .active // empty')
resolve_compact_config "${ACTIVE_MODEL:-}"

# 4.0 The threshold auto-compact hook. It runs before the request,
# not only after a step: that covers the first request after a new
# user message in an idle session. No-op when the trigger is cold
# or the cooldown from a failed summary call is active. The kill
# switch gates it. A failed compact leaves its marker in the log;
# the step proceeds in the current form. Its stdout is the status
# JSON for pipe consumers. The session log holds the record. The
# loop must not leak the line to the user terminal.
if [[ "$COMPACT_ENABLED" == "true" ]]; then
  "$COMPACT_BIN" "$SESSION_DIR" --config "$CONFIG" --reason threshold > /dev/null || true
fi

"$ASSEMBLE_BIN" "${ASSEMBLE_ARGS[@]}" > "$WORKDIR/model-request.json" || exit 1
if jq -e '.type == "error"' "$WORKDIR/model-request.json" > /dev/null; then
  "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/model-request.json" || exit 1
  exit 0
fi

# 4.5. The Exhausted form: the last-resort in-session compaction.
# It replaces the automatic handoff (correction 57): no new session,
# no context_exhausted marker. The force flag skips the kill switch
# and the cooldown. On success the re-projection below projects
# through the boundary. On failure the marker is in the log and the
# step proceeds in the current form: the model call runs and its
# failure path records the terminal event. The failed compact leaves
# the envelope in the request file: unwrap the embedded compact-
# candidate request so the model call sends a real request, not the
# envelope.
if jq -e '.type == "context_exhausted"' "$WORKDIR/model-request.json" > /dev/null; then
  if ! run_last_resort_compaction threshold; then
    jq -c '.request // empty' "$WORKDIR/model-request.json" > "$WORKDIR/model-request.json.unwrapped"
    if [[ -s "$WORKDIR/model-request.json.unwrapped" ]]; then
      mv "$WORKDIR/model-request.json.unwrapped" "$WORKDIR/model-request.json"
    else
      rm -f "$WORKDIR/model-request.json.unwrapped"
      exit 1
    fi
  fi
fi

# 5-6. model + parse, with the overflow recovery and the guard
# against empty assistant turns. An empty turn (no text and no
# tool calls) is a model glitch. Retry up to EMPTY_RETRIES times.
# A failed model call (API or stream error) is a transport problem
# unless the overflow classifier claims it. The overflow recovery
# runs once: one compact, one re-run. The second failure runs the
# last-resort compaction, then one more call. A failure after the
# last resort logs the terminal error event and stops the loop in
# the original session. No new session. No context_exhausted
# marker. The user reopens the same session.
EMPTY_RETRIES=3
MODEL_ERR_RETRIES=2
ATTEMPT=0
MODEL_ERR_ATTEMPT=0
OVF_RECOVERED=0   # the one overflow compact + re-run ran
LAST_RESORT=0     # the last-resort compaction ran
while true; do
  ATTEMPT=$((ATTEMPT + 1))

  # model: one API call. Capture exit code.
  set +e
  "$MODEL_BIN" --config "$CONFIG" < "$WORKDIR/model-request.json" > "$WORKDIR/model-output.json"
  MODEL_EXIT=$?
  set -e

  if [ "$MODEL_EXIT" -ne 0 ]; then
    # The last-resort failure: the terminal error event, no more
    # retries. The loop stops in the original session.
    if [ "$LAST_RESORT" = "1" ]; then
      ERROR_EVENT=$(jq -cn --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        '{v:1, type:"error", ts:$ts, message:"model binary failed after the last-resort compaction"}')
      echo "$ERROR_EVENT" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
      exit 0
    fi
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

  # The stop reason and the classification, before the transport
  # retry. The model guard: the classifier claims the failure only
  # when the request model matches the active config model. The
  # request JSON is the source: assemble writes model into every
  # request, and parse does not run on a failed call.
  MR=$(jq -r '.stop_reason // "none"' "$WORKDIR/model-output.json")
  REQ_MODEL=$(jq -r '.model // ""' "$WORKDIR/model-request.json")
  DETAIL=$(jq -r '.detail // ""' "$WORKDIR/model-output.json")
  if [ "$MR" = "error" ]; then
    OVERFLOW=0
    if [[ -n "$REQ_MODEL" && "$REQ_MODEL" == "$GUARD_MODEL" ]]; then
      if classify_overflow_error "$DETAIL"; then
        OVERFLOW=1
      fi
    fi
    if [ "$OVERFLOW" = "1" ]; then
      # The overflow recovery (plan 4.4 step 5). The error overflow
      # carries no strip: the failed response is not in the log,
      # and the last group is a valid step. Stripping it would
      # lose that step's context.
      if [ "$LAST_RESORT" = "1" ]; then
        # The second failure after the last-resort compaction.
        ERROR_EVENT=$(jq -cn --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg d "$DETAIL" \
          '{v:1, type:"error", ts:$ts, message:("context overflow: the last-resort compaction did not recover: " + $d)}')
        echo "$ERROR_EVENT" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
        exit 0
      fi
      if [[ "$COMPACT_ENABLED" == "true" && "$OVF_RECOVERED" = "0" ]]; then
        OVF_RECOVERED=1
        if "$COMPACT_BIN" "$SESSION_DIR" --config "$CONFIG" --reason overflow > /dev/null; then
          # The re-run: the re-projection projects through the new
          # boundary. The auto-continue: the interrupted turn
          # resumes in the same session.
          "$ASSEMBLE_BIN" "${ASSEMBLE_ARGS[@]}" > "$WORKDIR/model-request.json" || exit 1
          continue
        fi
      fi
      # The last-resort compaction: the overflow compact failed, the
      # kill switch is off, or this is the second overflow. The
      # forced flag skips the switch and the cooldown.
      LAST_RESORT=1
      run_last_resort_compaction overflow || true
      continue
    fi
  fi

  # Transport-level model failure (API error, truncated stream).
  # The model binary reports it with stop_reason "error" and a
  # detail. Retry while attempts remain. Then log the detail and
  # stop. The last-resort failure stops without the retry.
  if [ "$MR" = "error" ]; then
    if [ "$LAST_RESORT" = "1" ]; then
      ERROR_EVENT=$(jq -cn --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg d "$DETAIL" \
        '{v:1, type:"error", ts:$ts, message:("model API call failed after the last-resort compaction: " + $d)}')
      echo "$ERROR_EVENT" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
      exit 0
    fi
    if [ "$MODEL_ERR_ATTEMPT" -lt "$MODEL_ERR_RETRIES" ]; then
      MODEL_ERR_ATTEMPT=$((MODEL_ERR_ATTEMPT + 1))
      sleep 3
      continue
    fi
    ERROR_EVENT=$(jq -cn --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg d "$DETAIL" \
      '{v:1, type:"error", ts:$ts, message:("model API call failed after retries: " + $d)}')
    echo "$ERROR_EVENT" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
    exit 0
  fi

  # The silent overflow: a successful call whose measured input
  # tokens meet or exceed the input budget. Compact only, no
  # re-run: the valid response is on the log, and the loop
  # continues by its own state machine (plan 4.4 step 5, the pi
  # split: retry only when the stop reason is not stop). The kill
  # switch off sends the path to the last-resort compaction.
  MEASURED_IN=$(jq -r '.usage.input_tokens // 0' "$WORKDIR/model-output.json")
  if [[ -n "$REQ_MODEL" && "$REQ_MODEL" == "$GUARD_MODEL" && "$MEASURED_IN" =~ ^[0-9]+$ && "$MEASURED_IN" -ge "$INPUT_BUDGET" ]]; then
    if [[ "$COMPACT_ENABLED" == "true" ]]; then
      "$COMPACT_BIN" "$SESSION_DIR" --config "$CONFIG" --reason overflow > /dev/null || true
    else
      run_last_resort_compaction overflow || true
    fi
  fi

  # parse: validate, emit events, choose exit code.
  set +e
  "$BIN_DIR/parse" --config "$CONFIG" < "$WORKDIR/model-output.json" > "$WORKDIR/parsed.jsonl"
  PARSE_EXIT=$?
  set -e

  # Detect an empty assistant turn: no text and no tool calls.
  EMPTY=$(jq -c 'select(.type == "assistant_message") | select(((.content // "") | length) == 0) | select(((.tool_calls // []) | length) == 0) | "empty"' "$WORKDIR/parsed.jsonl" | head -1)
  LAST_STOP=$(jq -r 'select(.type == "assistant_message") | .stop_reason // ""' "$WORKDIR/parsed.jsonl" | head -1)
  OUT_TOK=$(jq -r '.usage.output_tokens // 0' "$WORKDIR/model-output.json")

  # The length-stop recovery (plan 4.4): a length stop with output
  # below the configured max output is recoverable, once. The output
  # is the provider stop, not the call count: a truncated tool-call
  # group recovers too. That includes the truncation stop (the
  # zero-output case with the input filling the window) and the
  # empty-content case the old script logged as terminal. The group
  # lands in the log before the retry: the assistant message, the
  # {}-argument calls, and the truncation-notice results. The strip
  # excludes it from the retry request. The route does not run on
  # this path. The terminal empty-content-length error moves behind
  # the recovery.
  if [[ "$LAST_STOP" = "length" && "$OUT_TOK" =~ ^[0-9]+$ && "$OUT_TOK" -lt "$MAX_OUT" ]]; then
    if [[ "$OVF_RECOVERED" = "0" ]]; then
      OVF_RECOVERED=1
      # Log the truncated group before the retry.
      "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/parsed.jsonl" || exit 1
      if [[ "$COMPACT_ENABLED" == "true" ]]; then
        "$COMPACT_BIN" "$SESSION_DIR" --config "$CONFIG" --reason overflow --strip-last-assistant > /dev/null || true
      else
        run_last_resort_compaction overflow || true
      fi
      "$ASSEMBLE_BIN" "${ASSEMBLE_ARGS[@]}" > "$WORKDIR/model-request.json" || exit 1
      continue
    fi
    # The second length stop: the last-resort compaction, then one
    # more call. A third failure stops the loop.
    LAST_RESORT=1
    run_last_resort_compaction overflow || true
    "$ASSEMBLE_BIN" "${ASSEMBLE_ARGS[@]}" > "$WORKDIR/model-request.json" || exit 1
    continue
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
  append_loop_phase tools
  jq -c 'select(.type == "tool_call")' "$WORKDIR/parsed.jsonl" \
    | route_cmd > "$WORKDIR/routed.jsonl" || exit 1
  cat "$WORKDIR/parsed.jsonl" "$WORKDIR/routed.jsonl" | "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" || exit 1
else
  "$BIN_DIR/log" --session "$SESSION_DIR" --schemas "$SCHEMA_DIR" < "$WORKDIR/parsed.jsonl" || exit 1
fi

exit 0
