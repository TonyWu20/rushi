#!/usr/bin/env bash
# The run.idle continue e2e (docs/loop-lifecycle-hooks.md 3.6;
# docs/pi-goal-readiness.md G3). Drives `harness run` with a stub
# model and the compiled goal hooks. Asserts the goal-continuation
# loop: a `continue` decision appends a `follow` user_message and the
# loop keeps working; budget exhaustion marks the goal blocked and
# stops the loop; a closed or absent goal stops immediately.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$ROOT/target/debug"

cargo build --quiet || { echo "FAIL: cargo build"; exit 1; }

PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); }
ko() {
  FAIL=$((FAIL + 1))
  echo "FAIL: $1"
}

# ── The work config ───────────────────────────────────────────────
# window 8000, output 4096. compact disabled so no compaction fires.
# The [[hooks.on]] entries register the compiled goal hooks on their
# windows: run.idle (goal-idle), compact.before (goal-compact),
# tool.before (goal-tools).
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
tools_root = "$ROOT/tools"

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
command = "$BIN_DIR/harness-hook-goal-idle"
args    = []

[[hooks.on]]
window  = "compact.before"
command = "$BIN_DIR/harness-hook-goal-compact"
args    = []

[[hooks.on]]
window  = "tool.before"
command = "$BIN_DIR/harness-hook-goal-tools"
args    = []
EOF
}

# ── The session builder ───────────────────────────────────────────
# One user message: claim is `awaiting_model`, the step runs one
# model call, then the turn ends idle.
seed_session() {
  cat > "$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"do the task"}
EOF
}

# Seed a goal.json with the given budget_tokens.
seed_goal() {
  cat > "$SESSIONS_DIR/goal.json" <<EOF
{"goal":"finish the task","active":true,"budget_tokens":$1,"used_tokens":0}
EOF
}

# ── The stub model ─────────────────────────────────────────────────
# Records every normal request on STUB_REQLOG, one JSON line each,
# and answers a final stop with a fixed usage reading.
make_stub() {
  cat > "$WORK/stub-model" <<'EOF'
#!/usr/bin/env bash
set -u
if [[ "${1:-}" == "--describe" ]]; then
  echo '{"active":"stub","model_id":"stub-model","reasoning_effort":"none","thinking_level":"off"}'
  exit 0
fi
req=$(cat)
printf '%s\n' "$req" >>"${STUB_REQLOG:-/dev/null}"
echo '{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":10}}'
EOF
  chmod +x "$WORK/stub-model"
}

run_harness() {
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_REQLOG="$WORK/reqlog"
    : >"$STUB_REQLOG"
    timeout 120 "$BIN_DIR/harness" run session
  ) >/dev/null 2>&1
  true
}

count_type() {
  jq -c "select(.type == \"$1\")" "$SLOG" 2>/dev/null | wc -l
}

count_follow_msgs() {
  jq -c 'select(.type == "user_message" and .queue == "follow")' "$SLOG" 2>/dev/null | wc -l
}

goal_field() {
  jq -r ".${1} // empty" "$SESSIONS_DIR/goal.json" 2>/dev/null
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
  WORK="$ROOT/scratch/e2e-run-idle/$1"
  rm -rf "$WORK"
  SESSIONS_DIR="$WORK/sessions/session"
  SLOG="$SESSIONS_DIR/events.jsonl"
  STUB_REQLOG="$WORK/reqlog"
  mkdir -p "$SESSIONS_DIR"
  echo "==== scenario: $1"
}

# ── Scenario 1: no goal file ──────────────────────────────────────
# No goal.json: the run.idle hook returns no decision, the loop
# stops after one model call. No follow messages, no goal state.
scenario_no_goal() {
  NEW_WORK no-goal
  work_config
  seed_session
  make_stub
  run_harness
  assert_eq "$(count_type assistant_message)" 1 "one model call"
  assert_eq "$(count_follow_msgs)" 0 "no follow user_message"
  [ ! -f "$SESSIONS_DIR/goal.json" ] && ok || ko "goal.json must not exist"
}

# ── Scenario 2: budgeted goal continues, then exhausts ────────────
# goal.json is active with a budget of 30; the stub reports 10
# output tokens per turn. The hook continues while budget remains,
# then marks the goal blocked on exhaustion and the loop stops.
scenario_budgeted_goal() {
  NEW_WORK budget
  work_config
  seed_session
  seed_goal 30
  make_stub
  run_harness
  # Three model calls: after the third, used reaches 30 (budget).
  assert_eq "$(count_type assistant_message)" 3 "three model calls until budget"
  # Two follow messages: after calls 1 and 2 the hook continues.
  assert_eq "$(count_follow_msgs)" 2 "two follow user_messages"
  assert_eq "$(goal_field blocked)" "true" "goal marked blocked on exhaustion"
  assert_eq "$(goal_field used_tokens)" "30" "used_tokens reached the budget"
}

# ── Scenario 3: completed goal stops immediately ──────────────────
# A closed goal (completed) in goal.json: the hook returns no
# decision, the loop stops after one model call.
scenario_closed_goal() {
  NEW_WORK closed-goal
  work_config
  seed_session
  cat > "$SESSIONS_DIR/goal.json" <<'EOF'
{"goal":"old task","active":false,"completed":true,"used_tokens":0}
EOF
  make_stub
  run_harness
  assert_eq "$(count_type assistant_message)" 1 "one model call"
  assert_eq "$(count_follow_msgs)" 0 "no follow user_message"
}

scenario_no_goal
scenario_budgeted_goal
scenario_closed_goal

echo
echo "run-idle-continue-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
