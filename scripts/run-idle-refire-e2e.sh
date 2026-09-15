#!/usr/bin/env bash
# e2e for the run idle refire flag, issue 6

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

# $1: optional extra config text (e.g. a [run] section),
# $2: optional extra [[hooks.on]] text (e.g. the model.before stub)
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
${1:-}
${2:-}
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

# The gate: on its first firing it stores a pending-feedback marker
# (simulating the writing-rule gate linting the last reply) and asks
# for a silent refire.
make_hook_refire() {
  printf '%s\n' '#!/usr/bin/env bash' > "$WORK/hook-run-idle"
  cat >> "$WORK/hook-run-idle" <<EOF
set -u
cat > /dev/null
n=\$(( \$(cat "$COUNT" 2>/dev/null || echo 0) + 1 ))
echo "\$n" > "$COUNT"

if [[ \$n -eq 1 ]]; then
  echo "ending replies without a question mark" > "$WORK/pending_feedback"
  echo '{"decision":"continue","payload":{"log_message":false,"refire":true}}'
else
  echo '{"decision":"stop"}'
fi
EOF
  chmod +x "$WORK/hook-run-idle"
}

# The gate keeps demanding a refire: used to prove the per-run cap
# stops the silent loop.
make_hook_refire_always() {
  printf '%s\n' '#!/usr/bin/env bash' > "$WORK/hook-run-idle"
  cat >> "$WORK/hook-run-idle" <<EOF
set -u
cat > /dev/null
n=\$(( \$(cat "$COUNT" 2>/dev/null || echo 0) + 1 ))
echo "\$n" > "$COUNT"

echo '{"decision":"continue","payload":{"log_message":false,"refire":true}}'
EOF
  chmod +x "$WORK/hook-run-idle"
}

# The plain silent continue of issue 4: no refire flag.
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

# The model.before transform: when a pending-feedback marker exists,
# it delivers it to the refired call as a prompt fragment, leaving a
# marker string in the request the stub model writes to the reqlog.
make_hook_model_before() {
  printf '%s\n' '#!/usr/bin/env bash' > "$WORK/hook-model-before"
  cat >> "$WORK/hook-model-before" <<EOF
set -u
in=\$(cat)
if [[ -f "$WORK/pending_feedback" ]]; then
  fb=\$(cat "$WORK/pending_feedback")
  rm -f "$WORK/pending_feedback"
  printf '%s' "\$in" | jq -c --arg fb "\$fb" \
    '{decision: "transform", payload: {request: (.request + {prompt_fragments: [["refire_fb", ("REFIRE-FEEDBACK: " + \$fb)]]})}}'
fi
EOF
  chmod +x "$WORK/hook-model-before"
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

req_line_has() {
  # $1: line number of the reqlog, $2: needle
  sed -n "$1p" "$STUB_REQLOG" 2>/dev/null | grep -qF "$2"
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
  WORK="$ROOT/scratch/e2e-run-idle-refire/$1"
  rm -rf "$WORK"
  SESSIONS_DIR="$WORK/sessions/session"
  SLOG="$SESSIONS_DIR/events.jsonl"
  STUB_REQLOG="$WORK/reqlog"
  COUNT="$WORK/idle-count"
  mkdir -p "$SESSIONS_DIR"
  echo "==== scenario: $1"
}

# Silent refire: two model turns, zero follow user_messages, and the
# second request carries the feedback delivered by model.before.
scenario_refire() {
  NEW_WORK refire
  work_config "" "[[hooks.on]]
window  = \"model.before\"
command = \"$WORK/hook-model-before\"
args    = []"
  seed_session
  make_stub
  make_hook_refire
  make_hook_model_before
  run_loop
  assert_eq "$(claim_state)" "idle" "the run loop ends at idle"
  assert_eq "$(user_message_count)" "1" \
    "only the seed user_message, no follow appended"
  assert_eq "$(follow_user_message)" "" \
    "no follow user_message was appended"
  assert_eq "$(wc -l < "$STUB_REQLOG" | tr -d ' ')" "2" \
    "the seed turn plus one silent refire turn"
  assert_eq "$(count_markers "run.refire")" "1" \
    "one refire marker"
  assert_eq "$(count_markers "run.refire_cap")" "0" \
    "the cap was not reached"
  assert_eq "$(count_markers "hook.model.before.transform")" "1" \
    "the feedback transform fired once"
  if req_line_has 2 "REFIRE-FEEDBACK"; then ok; else ko "the refired request carries the feedback marker"; fi
  if req_line_has 1 "REFIRE-FEEDBACK"; then ko "the first request must not carry the feedback"; else ok; fi
  assert_eq "$(cat "$COUNT")" "2" "the hook fired twice"
}

# The gate keeps demanding refires: the default cap (2) bounds the
# silent loop and the run stops cleanly.
scenario_cap_default() {
  NEW_WORK cap-default
  work_config
  seed_session
  make_stub
  make_hook_refire_always
  run_loop
  assert_eq "$(claim_state)" "idle" "the run loop ends at idle"
  assert_eq "$(user_message_count)" "1" \
    "no user_message appended by refires"
  assert_eq "$(wc -l < "$STUB_REQLOG" | tr -d ' ')" "3" \
    "the seed turn plus the two allowed refires"
  assert_eq "$(count_markers "run.refire")" "2" \
    "two refire markers under the default cap"
  assert_eq "$(count_markers "run.refire_cap")" "1" \
    "the cap marker was logged"
  assert_eq "$(cat "$COUNT")" "3" \
    "the hook fired three times, the third request was refused"
}

# The cap is configurable: [run] max_silent_refires = 1.
scenario_cap_one() {
  NEW_WORK cap-one
  work_config "[run]
max_silent_refires = 1"
  seed_session
  make_stub
  make_hook_refire_always
  run_loop
  assert_eq "$(claim_state)" "idle" "the run loop ends at idle"
  assert_eq "$(wc -l < "$STUB_REQLOG" | tr -d ' ')" "2" \
    "the seed turn plus the single allowed refire"
  assert_eq "$(count_markers "run.refire")" "1" \
    "one refire marker under cap 1"
  assert_eq "$(count_markers "run.refire_cap")" "1" \
    "the cap marker was logged"
}

# No refire flag: the issue-4 silent-continue path is unchanged.
scenario_no_flag() {
  NEW_WORK no-flag
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
    "one model turn only, no refire"
  assert_eq "$(count_markers "run.refire")" "0" \
    "no refire marker"
  assert_eq "$(count_markers "run.refire_cap")" "0" \
    "no cap marker"
  assert_eq "$(cat "$COUNT")" "2" \
    "the hook fired twice, the loop stayed alive"
}

scenario_refire
scenario_cap_default
scenario_cap_one
scenario_no_flag

echo
echo "run-idle-refire-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
