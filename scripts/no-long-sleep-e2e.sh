#!/usr/bin/env bash
# e2e for `harness-hook-no-long-sleep` (the tool.before guard against
# long blocking waits). A stub model asks for a 300s `sleep`; the hook
# blocks it via `blocked_calls` and the loop logs a synthetic failed
# tool_result carrying the monitor-pattern reason. The model is called
# a second time (the tool_result is owed a response) and stops.
#
# Proves the full path: tool.before window fires, the guard's
# `blocked_calls` are honored, the synthetic result is is_error, and
# the `hook.tool.before` resolution marker is `block`.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$ROOT/target/debug"

cargo build --quiet || { echo "FAIL cargo build"; exit 1; }

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); }
ko() { FAIL=$((FAIL + 1)); echo "FAIL $1"; }

WORK="$ROOT/scratch/e2e-no-long-sleep"
rm -rf "$WORK"
SESSIONS_DIR="$WORK/sessions/session"
SLOG="$SESSIONS_DIR/events.jsonl"
mkdir -p "$SESSIONS_DIR"

work_config() {
  cat > "$WORK/config.toml" <<EOF
[model]
api = "responses"
max_output_tokens = 4096

[model.stub]
model_id = "stub-model"
base_url = "http://127.0.0.1:1"
api_key_env = "DUMMY"
context_tokens = 8000

[active]
model = "stub"

[paths]
sessions_root = "sessions"
native_tool_paths = ["$ROOT/tools"]

[limits]
context_budget_tokens = 8000
compact_enabled = false

[system_prompt]
text = "test"

[hooks]
timeout_ms = 30000

[hooks.defs.hook-no-long-sleep]
command = "$BIN_DIR/harness-hook-no-long-sleep"

[hooks.pipeline."tool.before"]
steps = ["hook-no-long-sleep"]
EOF
}

seed_session() {
  cat > "$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"watch the build"}
EOF
}

# Stub model: first request asks for a 300s sleep; later requests stop.
make_stub() {
  printf '%s\n' '#!/usr/bin/env bash' > "$WORK/stub-model"
  cat >> "$WORK/stub-model" <<'EOF'
set -u

if [[ "${1:-}" == "--describe" ]]; then
  echo '{"active":"stub","model_id":"stub-model","reasoning_effort":"none","thinking_level":"off"}'
  exit 0
fi

req=$(cat)
printf '%s\n' "$req" >>"${STUB_REQLOG:-/dev/null}"
n=$(( $(cat "${STUB_COUNTER:-/dev/null}" 2>/dev/null || echo 0) + 1 ))
echo "$n" > "${STUB_COUNTER:-/dev/null}"

if [[ "$n" -le 1 ]]; then
  echo '{"text":"","tool_calls":[{"id":"c-sleep","name":"bash","arguments":"{\"command\":\"sleep 300 && squeue\"}"}],"reasoning":[],"stop_reason":"tool_calls","usage":{"input_tokens":10,"output_tokens":10}}'
else
  echo '{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":10}}'
fi
EOF
  chmod +x "$WORK/stub-model"
}

run_loop() {
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_REQLOG="$WORK/reqlog"
    export STUB_COUNTER="$WORK/counter"
    : > "$STUB_REQLOG"
    : > "$STUB_COUNTER"
    timeout 120 "$BIN_DIR/rushi" run session
  ) >/dev/null 2>&1
  true
}

scenario_block_long_sleep() {
  echo "==== scenario: long sleep is blocked by the guard"
  work_config
  seed_session
  make_stub
  run_loop

  # The loop must have stopped cleanly.
  jq -e '.state == "idle"' <( "$BIN_DIR/claim" --session "$SESSIONS_DIR" ) >/dev/null \
    && ok || ko "the run loop ends at idle"

  # The blocked call produced a synthetic failed tool_result.
  local res
  res=$(jq -c 'select(.type == "tool_result" and .id == "c-sleep")' "$SLOG" 2>/dev/null)
  [[ -n "$res" ]] && ok || ko "a synthetic tool_result exists for the blocked call"
  echo "$res" | jq -e '.is_error == true' >/dev/null \
    && ok || ko "the synthetic result is is_error"
  echo "$res" | jq -r '.value.text' | rg -q 'LONG SLEEP DETECTED' \
    && ok || ko "the reason names the long-sleep detection"
  echo "$res" | jq -r '.value.text' | rg -q 'rushi docs monitoring' \
    && ok || ko "the reason points at the monitor pattern"

  # The window resolution marker is `block`.
  local marker
  marker=$(jq -r 'select(.type == "ext_status" and .id == "hook.tool.before") | .value' "$SLOG" 2>/dev/null | head -1)
  [[ "$marker" == "block" ]] && ok || ko "hook.tool.before resolution is block (got [$marker])"

  # The real bash tool never ran: no result carries a bash exit_code.
  ! jq -e 'select(.type == "tool_result" and .id == "c-sleep") | .value | has("exit_code")' "$SLOG" >/dev/null \
    && ok || ko "no real bash output reached the log"
}

scenario_short_sleep_passes() {
  echo "==== scenario: a short sleep is not blocked"
  work_config
  seed_session
  make_stub_short
  run_loop

  local res
  res=$(jq -c 'select(.type == "tool_result" and .id == "c-short")' "$SLOG" 2>/dev/null)
  [[ -n "$res" ]] && ok || ko "a tool_result exists for the short sleep"
  # A real bash result carries an exit_code (the call was not blocked).
  echo "$res" | jq -e '(.value.details // .value) | has("exit_code")' >/dev/null \
    && ok || ko "the short sleep ran (result carries an exit_code)"
  # No block marker for this window.
  ! jq -e 'select(.type == "ext_status" and .id == "hook.tool.before" and .value == "block")' "$SLOG" >/dev/null \
    && ok || ko "no block marker for a short sleep"
}

make_stub_short() {
  printf '%s\n' '#!/usr/bin/env bash' > "$WORK/stub-model"
  cat >> "$WORK/stub-model" <<'EOF'
set -u

if [[ "${1:-}" == "--describe" ]]; then
  echo '{"active":"stub","model_id":"stub-model","reasoning_effort":"none","thinking_level":"off"}'
  exit 0
fi

req=$(cat)
printf '%s\n' "$req" >>"${STUB_REQLOG:-/dev/null}"
n=$(( $(cat "${STUB_COUNTER:-/dev/null}" 2>/dev/null || echo 0) + 1 ))
echo "$n" > "${STUB_COUNTER:-/dev/null}"

if [[ "$n" -le 1 ]]; then
  echo '{"text":"","tool_calls":[{"id":"c-short","name":"bash","arguments":"{\"command\":\"sleep 2 && echo built\"}"}],"reasoning":[],"stop_reason":"tool_calls","usage":{"input_tokens":10,"output_tokens":10}}'
else
  echo '{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":10}}'
fi
EOF
  chmod +x "$WORK/stub-model"
}

scenario_block_long_sleep
scenario_short_sleep_passes

echo "no-long-sleep-e2e: $PASS passed, $FAIL failed"
[[ "$FAIL" -eq 0 ]] || exit 1
