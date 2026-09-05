#!/usr/bin/env bash
# The run.idle continue e2e (docs/loop-lifecycle-hooks.md 3.6;
# docs/goal-ux.md §3 step 7). Drives `rushi run` with a stub model
# and the compiled goal hooks. Asserts the goal-continuation loop:
# an active goal continues via run.idle; goal_complete / goal_blocked
# close it; paused or cleared goals stop the loop. No budget cap
# (docs/goal-ux.md §1.6).

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
window  = "model.before"
command = "$BIN_DIR/harness-hook-goal-arm"
args    = []

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

# Seed a goal.json with a given id. No budget (docs/goal-ux.md §1.6).
seed_goal() {
  local id="${1:-g-test0001}"
  cat > "$SESSIONS_DIR/goal.json" <<EOF
{"id":"$id","goal":"finish the task","active":true,"used_tokens":0,"iteration":0,"completed":false,"blocked":false,"block_reason":null,"opened_at":"t+0s","closed_at":null}
EOF
}

# Seed a goal.json with specific state fields.
seed_goal_state() {
  cat > "$SESSIONS_DIR/goal.json" <<EOF
$1
EOF
}

# ── Stub model variants ───────────────────────────────────────────

# Simple stub: always returns stop, no tool calls.
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

# Goal-complete stub: returns goal_complete tool_call on exactly the
# Nth call, stop before and after. Expects GOAL_ID env var for the
# id and GOAL_SUMMARY for the summary text.
make_goal_complete_stub() {
  local complete_after="${1:-3}"
  cat > "$WORK/stub-model" <<EOF
#!/usr/bin/env bash
set -u
if [[ "\${1:-}" == "--describe" ]]; then
  echo '{"active":"stub","model_id":"stub-model","reasoning_effort":"none","thinking_level":"off"}'
  exit 0
fi
req=\$(cat)
printf '%s\n' "\$req" >>"\${STUB_REQLOG:-/dev/null}"
n=\$(wc -l < "\${STUB_REQLOG}" 2>/dev/null || echo 0)
if [[ "\$n" -eq $complete_after ]]; then
  printf '{"text":"done","tool_calls":[{"name":"goal_complete","id":"call-complete","arguments":{"goal_id":"%s","summary":"%s"}}],"reasoning":[],"stop_reason":"tool_calls","usage":{"input_tokens":10,"output_tokens":10}}\n' "\${GOAL_ID:-g-test0001}" "\${GOAL_SUMMARY:-all requirements verified}"
else
  echo '{"text":"working","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":10}}'
fi
EOF
  chmod +x "$WORK/stub-model"
}

run_harness() {
  local t="${1:-120}"
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_REQLOG="$WORK/reqlog"
    : >"$STUB_REQLOG"
    export GOAL_ID="${GOAL_ID:-g-test0001}"
    export GOAL_SUMMARY="${GOAL_SUMMARY:-all requirements verified}"
    timeout "$t" "$BIN_DIR/rushi" run session
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
  # No `// empty`: jq treats `false` as falsy, and goal states use
  # boolean fields that must surface as "false", not empty.
  jq -r ".${1}" "$SESSIONS_DIR/goal.json" 2>/dev/null
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
  GOAL_ID=""
  GOAL_SUMMARY=""
  mkdir -p "$SESSIONS_DIR"
  echo "==== scenario: $1"
}

# ── Scenario: no goal file (P11) ──────────────────────────────────
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

# ── Scenario: active goal continues, then completes (P10) ───────
# Active goal with no budget: the loop continues until the model
# calls goal_complete with the correct id. The continuation prompts
# contain "continuation #N".
scenario_active_goal_continues() {
  NEW_WORK active-goal
  work_config
  seed_session
  seed_goal "g-test0001"
  make_goal_complete_stub 3
  GOAL_ID="g-test0001"
  GOAL_SUMMARY="all requirements verified"
  run_harness
  # Four model calls: calls 1-2 stop, call 3 triggers goal_complete,
  # call 4 is the final stop after the tool result is processed.
  assert_eq "$(count_type assistant_message)" 4 "four model calls"
  # Two follow messages: after calls 1 and 2 the hook continues.
  assert_eq "$(count_follow_msgs)" 2 "two follow user_messages"
  # Goal completed.
  assert_eq "$(goal_field completed)" "true" "goal marked completed"
  assert_eq "$(goal_field active)" "false" "goal no longer active"
  # Continuation prompt contains the goal text and continuation number.
  jq -c 'select(.type == "user_message" and .queue == "follow")' "$SLOG" 2>/dev/null \
    | head -1 | grep -q "continuation #1" && ok || ko "first follow msg missing continuation #1"
  jq -c 'select(.type == "user_message" and .queue == "follow")' "$SLOG" 2>/dev/null \
    | sed -n '2p' | grep -q "continuation #2" && ok || ko "second follow msg missing continuation #2"
}

# ── Scenario: completed goal stops immediately (P11) ─────────────
scenario_closed_goal() {
  NEW_WORK closed-goal
  work_config
  seed_session
  seed_goal_state '{"id":"g-old0001","goal":"old task","active":false,"used_tokens":0,"iteration":0,"completed":true,"blocked":false,"block_reason":null,"opened_at":"t+0s","closed_at":"t+100s"}'
  make_stub
  run_harness
  assert_eq "$(count_type assistant_message)" 1 "one model call"
  assert_eq "$(count_follow_msgs)" 0 "no follow user_message"
}

# ── Scenario: goal_complete with wrong goal_id (P8) ──────────────
# The model calls goal_complete with a mismatched id. The tool
# rejects it; goal.json stays active. The loop keeps going until
# timeout (no budget cap).
scenario_goal_complete_wrong_id() {
  NEW_WORK wrong-id
  work_config
  seed_session
  seed_goal "g-test0001"
  make_goal_complete_stub 1
  GOAL_ID="g-wrongid9"
  GOAL_SUMMARY="all requirements verified"
  # Short timeout: the rejected tool_result is recorded on the first pass;
  # the loop would otherwise continue forever (no budget cap).
  run_harness 20
  # The tool_call was rejected: goal.json is still active.
  assert_eq "$(goal_field active)" "true" "goal still active after wrong-id rejection"
  assert_eq "$(goal_field completed)" "false" "goal not completed"
  # The tool_result should carry an error.
  jq -c 'select(.type == "tool_result" and .is_error == true)' "$SLOG" 2>/dev/null \
    | grep -qi "goal_id does not match" && ok || ko "tool_result missing goal_id mismatch message"
}

# ── Scenario: goal_complete with contradictory summary (P9) ──────
# The model calls goal_complete with a summary that contradicts
# completion. The tool rejects it; goal.json stays active.
scenario_goal_complete_contradictory() {
  NEW_WORK contradictory
  work_config
  seed_session
  seed_goal "g-test0001"
  make_goal_complete_stub 1
  GOAL_ID="g-test0001"
  GOAL_SUMMARY="not complete, still failing"
  # Short timeout: the rejected tool_result is recorded on the first pass;
  # the loop would otherwise continue forever (no budget cap).
  run_harness 20
  # The tool_call was rejected: goal.json is still active.
  assert_eq "$(goal_field active)" "true" "goal still active after contradictory rejection"
  assert_eq "$(goal_field completed)" "false" "goal not completed"
  # The tool_result should carry an error mentioning the contradiction.
  jq -c 'select(.type == "tool_result" and .is_error == true)' "$SLOG" 2>/dev/null \
    | grep -qi "rejected\|contradict" && ok || ko "tool_result missing contradiction rejection"
}

# ── Scenario: paused goal stops (P3) ─────────────────────────────
# A goal with active=false (paused): the run.idle hook returns {},
# the loop stops.
scenario_goal_paused() {
  NEW_WORK paused
  work_config
  seed_session
  seed_goal_state '{"id":"g-pause001","goal":"paused task","active":false,"used_tokens":500,"iteration":3,"completed":false,"blocked":false,"block_reason":null,"opened_at":"t+0s","closed_at":null}'
  make_stub
  run_harness
  assert_eq "$(count_type assistant_message)" 1 "one model call (paused goal stops)"
  assert_eq "$(count_follow_msgs)" 0 "no follow user_message"
  assert_eq "$(goal_field active)" "false" "goal remains paused"
}

# ── Scenario: blocked goal stops, resume continues (P4) ─────────
scenario_goal_blocked_stops() {
  NEW_WORK blocked-stops
  work_config
  seed_session
  seed_goal_state '{"id":"g-block001","goal":"blocked task","active":false,"used_tokens":200,"iteration":5,"completed":false,"blocked":true,"block_reason":"missing dep","opened_at":"t+0s","closed_at":"t+50s"}'
  make_stub
  run_harness
  assert_eq "$(count_type assistant_message)" 1 "one model call (blocked goal stops)"
  assert_eq "$(count_follow_msgs)" 0 "no follow user_message"
}

# ── Scenario: cleared goal (P5) ──────────────────────────────────
# goal.json deleted: the run.idle hook returns {}, the loop stops.
scenario_goal_cleared() {
  NEW_WORK cleared
  work_config
  seed_session
  # No goal.json at all (already cleared).
  make_stub
  run_harness
  assert_eq "$(count_type assistant_message)" 1 "one model call (no goal)"
  assert_eq "$(count_follow_msgs)" 0 "no follow user_message"
  [ ! -f "$SESSIONS_DIR/goal.json" ] && ok || ko "goal.json should not exist"
}

# ── Scenario: goal block byte-stability (P16/P17) ────────────────
# Verify that the goal block is a pure function of (goal, id):
# two calls with the same goal+id but different iteration/used_tokens
# produce identical blocks.
scenario_goal_block_stable() {
  NEW_WORK block-stable
  work_config
  seed_session
  seed_goal "g-stable01"
  make_goal_complete_stub 3
  GOAL_ID="g-stable01"
  GOAL_SUMMARY="done"
  run_harness

  # Extract the goal block from the model requests: it is the last
  # message item in each request's input array.
  # Compare the goal block bytes across consecutive model calls.
  local blocks
  blocks=$(jq -c '.input[-1].content // empty' "$STUB_REQLOG" 2>/dev/null | sort -u)
  local unique_count
  unique_count=$(echo "$blocks" | grep -c . || true)
  # All calls should carry the identical goal block (P17).
  assert_eq "$unique_count" 1 "goal block byte-identical across turns (got $unique_count unique)"
}

scenario_no_goal
scenario_active_goal_continues
scenario_closed_goal
scenario_goal_complete_wrong_id
scenario_goal_complete_contradictory
scenario_goal_paused
scenario_goal_blocked_stops
scenario_goal_cleared
scenario_goal_block_stable

echo
echo "run-idle-continue-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
