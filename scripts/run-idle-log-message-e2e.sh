#!/usr/bin/env bash
# e2e for the run.idle pipeline window (docs/loop-lifecycle-hooks.md
# 12.5, issue #38; the log-message semantics of issue #4).
#
# Under the pipeline ABI the run.idle hook appends its own follow-up
# `user_message` to the session log through the `LOG_BIN` env var
# (the `log` stage binary); the loop drains pending messages as
# usual. An `abort` (exit 2) vetoes the default stop and keeps the
# loop alive; a `fail` (exit 3) logs `hook.run.idle.error` and the
# window resolves to its default (stop) — the loop never wedges
# (P4).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$ROOT/target/debug"

cargo build --quiet || {
  echo "FAIL cargo build"
  exit 1
}

PASS=0
FAIL=0
ok() {
  PASS=$((PASS + 1))
}
ko() {
  FAIL=$((FAIL + 1))
  echo "FAIL $1"
}

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

[limits]
context_budget_tokens = 8000
compact_reserve_tokens = 500
compact_keep_tokens = 2000
compact_enabled = false

[system_prompt]
text = "test"

[hooks]
timeout_ms = 30000

[hooks.defs.hook-run-idle]
command = "$WORK/hook-run-idle"

[hooks.pipeline."run.idle"]
steps = ["hook-run-idle"]
EOF
}

work_config_bare() {
  # No pipeline entry: the byte-identical no-hooks default (P1).
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

[limits]
context_budget_tokens = 8000
compact_reserve_tokens = 500
compact_keep_tokens = 2000
compact_enabled = false

[system_prompt]
text = "test"
EOF
}

seed_session() {
  cat > "$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"do the task"}
EOF
}

make_stub() {
  printf '%s\n' '#!/usr/bin/env bash' > "$WORK/stub-model"
  cat >> "$WORK/stub-model" <<'EOF'
set -u

if [[ "${1:-}" == "--describe" ]]; then
  jq -cn '{active: "stub", model_id: "stub-model", reasoning_effort: "none", thinking_level: "off"}'
  exit 0
fi

req=$(cat)
printf '%s\n' "$req" >>"${STUB_REQLOG:-/dev/null}"

jq -cn '{text: "done", tool_calls: [], reasoning: [], stop_reason: "stop", usage: {input_tokens: 10, output_tokens: 10}}'
EOF
  chmod +x "$WORK/stub-model"
}

# The hook appends its own follow user_message via LOG_BIN on the
# first firing (issue #4's plain behavior), then stops.
make_hook_plain() {
  printf '%s\n' '#!/usr/bin/env bash' > "$WORK/hook-run-idle"
  cat >> "$WORK/hook-run-idle" <<EOF
set -u
payload=\$(cat)
n=\$(( \$(cat "$COUNT" 2>/dev/null || echo 0) + 1 ))
echo "\$n" > "$COUNT"

if [[ \$n -eq 1 ]]; then
  ts=\$(date -u +%Y-%m-%dT%H:%M:%SZ)
  jq -cn --arg ts "\$ts" '{v:1, type:"user_message", ts:\$ts, content:"revise", queue:"follow"}' \
    | "\$LOG_BIN" --session "\$SESSION"
fi
echo '{}'
EOF
  chmod +x "$WORK/hook-run-idle"
}

# The veto hook (12.4): exit 2 on the first firing keeps the loop
# alive without appending a message; the second firing lets the
# loop stop.
make_hook_veto() {
  printf '%s\n' '#!/usr/bin/env bash' > "$WORK/hook-run-idle"
  cat >> "$WORK/hook-run-idle" <<EOF
set -u
cat > /dev/null
n=\$(( \$(cat "$COUNT" 2>/dev/null || echo 0) + 1 ))
echo "\$n" > "$COUNT"

if [[ \$n -eq 1 ]]; then
  echo '{"reason":"goal still open"}'
  exit 2
fi
echo '{}'
EOF
  chmod +x "$WORK/hook-run-idle"
}

# The failing hook (P4): exit 3 logs the error; the window resolves
# to its default (stop) and the loop stops.
make_hook_fail() {
  printf '%s\n' '#!/usr/bin/env bash' > "$WORK/hook-run-idle"
  cat >> "$WORK/hook-run-idle" <<EOF
set -u
cat > /dev/null
echo '{"reason":"idle hook crashed"}'
exit 3
EOF
  chmod +x "$WORK/hook-run-idle"
}

run_loop() {
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_REQLOG="$WORK/reqlog"
    : >"$STUB_REQLOG"
    timeout 120 "$BIN_DIR/rushi" run session
  ) >/dev/null 2>&1
  true
}

claim_state() {
  "$BIN_DIR/claim" --session "$SESSIONS_DIR" | jq -r .state
}

user_message_count() {
  jq -c 'select(.type == "user_message")' "$SLOG" 2>/dev/null | wc -l | tr -d ' '
}

follow_user_message() {
  jq -c 'select(.type == "user_message" and .queue == "follow")' "$SLOG" 2>/dev/null | head -1
}

count_markers() {
  local n
  n=$(jq -c "select(.type == \"ext_status\" and .id == \"$1\")" "$SLOG" 2>/dev/null | wc -l)
  echo "$n"
}

marker_values() {
  jq -c "select(.type == \"ext_status\" and .id == \"$1\") | .value" "$SLOG" 2>/dev/null
}

assert_eq() {
  local n
  if [ "$1" = "$2" ]; then
    ok
  else
    ko "$3: got [$1], want [$2]"
  fi
}

NEW_WORK() {
  WORK="$ROOT/scratch/e2e-run-idle-log-message/$1"
  rm -rf "$WORK"
  SESSIONS_DIR="$WORK/sessions/session"
  SLOG="$SESSIONS_DIR/events.jsonl"
  STUB_REQLOG="$WORK/reqlog"
  COUNT="$WORK/idle-count"
  mkdir -p "$SESSIONS_DIR"
  echo "==== scenario: $1"
}

# plain: the hook appends the follow user message; the loop drains
# it and runs a second turn, then stops.
scenario_plain() {
  NEW_WORK plain
  work_config
  seed_session
  make_stub
  make_hook_plain
  run_loop
  assert_eq "$(claim_state)" "idle" "the run loop ends at idle"
  assert_eq "$(user_message_count)" "2" \
    "the seed plus the follow user_message the hook appended"
  local follow
  follow="$(follow_user_message)"
  assert_eq "$(jq -r '.content' <<<"$follow")" "revise" \
    "the follow user_message carries the hook message"
  assert_eq "$(wc -l < "$STUB_REQLOG" | tr -d ' ')" "2" \
    "two model turns ran"
  assert_eq "$(cat "$COUNT")" "2" "the hook fired twice"
  assert_eq "$(count_markers "hook.run.idle.error")" "0" \
    "no failure marker for a successful hook"
  assert_eq "$(count_markers "hook.run.idle")" "2" \
    "one resolution marker per run.idle firing"
  assert_eq "$(marker_values "hook.run.idle" | sed -n 1p)" '"continue"' \
    "the first firing resolved to continue"
  assert_eq "$(marker_values "hook.run.idle" | sed -n 2p)" '"stop"' \
    "the second firing resolved to stop"
  assert_eq "$(jq -c 'select(.id == "hook.run.idle.chain") | .value.steps' "$SLOG" 2>/dev/null | head -1)" \
    '["noop"]' "the step emitted no state (the effect is the LOG_BIN append)"
}

# veto: an exit-2 abort vetoes the default stop. No message is
# appended; the loop stays alive for one more turn, then the
# hook lets it stop.
scenario_veto() {
  NEW_WORK veto
  work_config
  seed_session
  make_stub
  make_hook_veto
  run_loop
  assert_eq "$(claim_state)" "idle" "the run loop ends at idle"
  assert_eq "$(user_message_count)" "1" \
    "only the seed user_message (an abort appends nothing)"
  assert_eq "$(follow_user_message)" "" \
    "no follow user_message was appended"
  assert_eq "$(wc -l < "$STUB_REQLOG" | tr -d ' ')" "1" \
    "one model turn only"
  assert_eq "$(cat "$COUNT")" "2" \
    "the hook fired twice, the veto kept the loop alive"
  assert_eq "$(count_markers "hook.run.idle.error")" "0" \
    "an abort is not a failure marker"
  assert_eq "$(jq -c 'select(.id == "hook.run.idle.chain") | .value.steps' "$SLOG" 2>/dev/null | head -1)" \
    '["abort(goal still open)"]' \
    "the chain marker records the abort with its reason"
  assert_eq "$(jq -r 'select(.id == "hook.run.idle.chain") | .value.stop_kind' "$SLOG" 2>/dev/null | head -1)" \
    "abort" "the chain stopped on the veto"
}

# fail: an exit-3 step logs the error; the window default (stop)
# applies and the loop stops (P4: it never wedges).
scenario_fail() {
  NEW_WORK fail
  work_config
  seed_session
  make_stub
  make_hook_fail
  run_loop
  assert_eq "$(claim_state)" "idle" "the run loop ends at idle"
  assert_eq "$(user_message_count)" "1" \
    "only the seed user_message"
  assert_eq "$(wc -l < "$STUB_REQLOG" | tr -d ' ')" "1" \
    "one model turn only"
  assert_eq "$(count_markers "hook.run.idle.error")" "1" \
    "one error marker for the failed step"
  assert_eq "$(jq -c 'select(.id == "hook.run.idle.chain") | .value.steps' "$SLOG" 2>/dev/null)" \
    '["fail(idle hook crashed)"]' \
    "the chain marker records the fail with its detail"
  assert_eq "$(marker_values "hook.run.idle")" '"stop"' \
    "the failed chain resolved to the window default"
}

# default: no pipeline entry — zero steps, no markers, the loop
# stops after one turn (P1).
scenario_default() {
  NEW_WORK default
  work_config_bare
  seed_session
  make_stub
  run_loop
  assert_eq "$(claim_state)" "idle" "the run loop ends at idle"
  assert_eq "$(user_message_count)" "1" "only the seed user_message"
  assert_eq "$(wc -l < "$STUB_REQLOG" | tr -d ' ')" "1" \
    "one model turn only"
  assert_eq "$(count_markers "hook.run.idle")" "0" "no resolution marker"
  assert_eq "$(count_markers "hook.run.idle.chain")" "0" "no chain marker"
  assert_eq "$(count_markers "hook.run.idle.error")" "0" "no error marker"
}

scenario_plain
scenario_veto
scenario_fail
scenario_default

echo
echo "run-idle-log-message-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
