#!/usr/bin/env bash
# e2e for the run idle log message flag, issue 4

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

[[hooks.on]]
window  = "run.idle"
command = "$WORK/hook-run-idle"
args    = []
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

# The hook counts firings and stops on the second.
make_hook_plain() {
  printf '%s\n' '#!/usr/bin/env bash' > "$WORK/hook-run-idle"
  cat >> "$WORK/hook-run-idle" <<EOF
set -u
cat > /dev/null
n=\$(( \$(cat "$COUNT" 2>/dev/null || echo 0) + 1 ))
echo "\$n" > "$COUNT"

if [[ \$n -eq 1 ]]; then
  echo '{"decision":"continue","payload":{"message":"revise"}}'
else
  echo '{"decision":"stop"}'
fi
EOF
  chmod +x "$WORK/hook-run-idle"
}

make_hook_silent() {
  printf '%s\n' '#!/usr/bin/env bash' > "$WORK/hook-run-idle"
  cat >> "$WORK/hook-run-idle" <<EOF
set -u
cat > /dev/null
n=\$(( \$(cat "$COUNT" 2>/dev/null || echo 0) + 1 ))
echo "\$n" > "$COUNT"

if [[ \$n -eq 1 ]]; then
  echo '{"decision":"continue","payload":{"message":"revise","log_message":false}}'
else
  echo '{"decision":"stop"}'
fi
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

# plain appends the follow user message.
scenario_plain() {
  NEW_WORK plain
  work_config
  seed_session
  make_stub
  make_hook_plain
  run_loop
  assert_eq "$(claim_state)" "idle" "the run loop ends at idle"
  assert_eq "$(user_message_count)" "2" \
    "the seed plus the follow user_message"
  local follow
  follow="$(follow_user_message)"
  assert_eq "$(jq -r '.content' <<<"$follow")" "revise" \
    "the follow user_message carries the hook message"
  assert_eq "$(wc -l < "$STUB_REQLOG" | tr -d ' ')" "2" \
    "two model turns ran"
  assert_eq "$(cat "$COUNT")" "2" "the hook fired twice"
  assert_eq "$(count_markers "hook.run.idle.error")" "0" \
    "no failure marker for a successful hook"
}

# silent appends no user message but the loop stays alive.
scenario_silent() {
  NEW_WORK silent
  work_config
  seed_session
  make_stub
  make_hook_silent
  run_loop
  assert_eq "$(claim_state)" "idle" "the run loop ends at idle"
  assert_eq "$(user_message_count)" "1" \
    "only the seed user_message"
  assert_eq "$(follow_user_message)" "" \
    "no follow user_message was appended"
  assert_eq "$(wc -l < "$STUB_REQLOG" | tr -d ' ')" "1" \
    "one model turn only"
  assert_eq "$(cat "$COUNT")" "2" \
    "the hook fired twice, the loop stayed alive"
  assert_eq "$(count_markers "hook.run.idle.error")" "0" \
    "no failure marker for a successful hook"
}

scenario_plain
scenario_silent

echo
echo "run-idle-log-message-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
