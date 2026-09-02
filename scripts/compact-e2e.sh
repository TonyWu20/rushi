#!/usr/bin/env bash
# The auto-compact e2e suite (docs/auto-compact-plan.md section 5).
# It drives step.sh with a scriptable stub model binary and asserts
# the marker shapes on the session event log.
#
# The stub model reads the request JSON from stdin. Summary calls
# (tools: []) answer from STUB_SUMMARY, or fail/empty on the env
# flags. Normal calls answer from the STUB_PLAN lines in order.
# The stub records every summary request to STUB_REQLOG for the
# iterative assertions.

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

# ── The stub model ───────────────────────────────────────────────
make_stub() {
  cat > "$WORK/stub-model" <<'EOF'
#!/usr/bin/env bash
set -u
if [[ "${1:-}" == "--describe" ]]; then
  echo '{"active":"stub","model_id":"stub-model","reasoning_effort":"none","thinking_level":"off"}'
  exit 0
fi
req=$(cat)
if grep -q "summary now" <<<"$req"; then
  # The summary call: the ask item names it.
  if [[ -n "${STUB_REQLOG:-}" ]]; then
    printf '%s\n' "$req" >>"$STUB_REQLOG"
  fi
  if [[ "${STUB_SUMMARY_FAILS:-0}" == "1" ]]; then
    echo "stub: the summary call failed" >&2
    exit 1
  fi
  if [[ -n "${STUB_SUMMARY_FAIL_COUNT_FILE:-}" ]]; then
    # The fail-N fixture: the first N summary calls fail, then
    # success. The count file survives across the compact runs.
    local c limit
    c=$(cat "$STUB_SUMMARY_FAIL_COUNT_FILE" 2>/dev/null || echo 0)
    c=$((c + 1))
    echo "$c" >"$STUB_SUMMARY_FAIL_COUNT_FILE"
    limit="${STUB_SUMMARY_FAIL_N:-1}"
    if [ "$c" -le "$limit" ]; then
      echo "stub: the summary call failed ($c/$limit)" >&2
      exit 1
    fi
  fi
  if [[ "${STUB_SUMMARY_EMPTY:-0}" == "1" ]]; then
    echo '{"text":"","tool_calls":[],"reasoning":[],"stop_reason":"length","usage":{"input_tokens":10,"output_tokens":0}}'
    exit 0
  fi
  echo "${STUB_SUMMARY:-{\"text\":\"the summary of the old region\",\"tool_calls\":[],\"reasoning\":[],\"stop_reason\":\"stop\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}}"
  exit 0
fi
N=$(cat "${STUB_STATE:-}" 2>/dev/null || echo 0)
N=$((N + 1))
[ -n "${STUB_STATE:-}" ] && echo "$N" >"$STUB_STATE"
line=$(sed -n "${N}p" "${STUB_PLAN:-/dev/null}")
[[ -z "$line" ]] && line=$(tail -1 "${STUB_PLAN:-/dev/null}")
echo "$line"
exit 0
EOF
  chmod +x "$WORK/stub-model"
}

# ── The work config ──────────────────────────────────────────────
# window 8000, output 4096, input budget 3904, trigger 3404.
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
compact_enabled = $COMPACT_ENABLED

[system_prompt]
text = "test"
EOF
}

# ── The session builder ──────────────────────────────────────────
# A session that crosses the trigger level: three assistant turns,
# the usage readings above the 3404 trigger. The results carry
# 8000 chars each (2000 tokens) so the keep window (2000 tokens)
# leaves a non-empty old region: the cut snaps back to the third
# user turn and the first two groups stay in the old region.
seed_session() {
  # $1 = the first usage reading, $2 = the second, $3 = the third.
  local u1="$1" u2="$2" u3="${3:-4300}"
  local A B C
  A="$(printf 'a%.0s' {1..4000})$(printf 'a1%.0s' {1..4000})"
  B="$(printf 'b%.0s' {1..4000})$(printf 'b1%.0s' {1..4000})"
  C="$(printf 'c%.0s' {1..4000})$(printf 'c1%.0s' {1..4000})"
  cat > "$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"do the task, the old work"}
{"v":1,"type":"assistant_message","ts":"t2","seq":2,"content":"step one","reasoning":[],"tool_calls":[{"id":"c1","name":"read","arguments":{"file_path":"a.txt"}}],"usage":{"input_tokens":$u1,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t3","seq":3,"id":"c1","value":{"text":"$A"},"is_error":false}
{"v":1,"type":"stop","ts":"t4","seq":4,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t5","seq":5,"content":"continue"}
{"v":1,"type":"assistant_message","ts":"t6","seq":6,"content":"step two","reasoning":[],"tool_calls":[{"id":"c2","name":"bash","arguments":{"command":"ls"}}],"usage":{"input_tokens":$u2,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t7","seq":7,"id":"c2","value":{"text":"$B"},"is_error":false}
{"v":1,"type":"stop","ts":"t8","seq":8,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t8b","seq":9,"content":"carry on"}
{"v":1,"type":"assistant_message","ts":"t10","seq":10,"content":"step three","reasoning":[],"tool_calls":[{"id":"c3","name":"bash","arguments":{"command":"ls -l"}}],"usage":{"input_tokens":$u3,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t11","seq":11,"id":"c3","value":{"text":"$C"},"is_error":false}
{"v":1,"type":"stop","ts":"t12","seq":12,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t13","seq":13,"content":"new task"}
EOF
}

# Run one step against the work session with the stub.
run_step() {
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_PLAN="$WORK/plan" STUB_STATE="$WORK/stub-n"
    export STUB_REQLOG="${STUB_REQLOG:-$WORK/reqlog}"
    local step="$SCRIPT_DIR/step.sh"
    if [[ -n "${STEP_DEBUG:-}" ]]; then
      "$step" session
    else
      "$step" session 2>/dev/null
    fi
    true
  )
}

# The claim state of the work session.
claim_state() {
  "$BIN_DIR/claim" --session "$SESSIONS_DIR" | jq -r .state
}

# The marker counts.
count_events() {
  local n
  n=$(jq -c "select(.type == \"$1\")" "$SLOG" 2>/dev/null | wc -l)
  echo "$n"
}

# The last marker of a type.
last_event() {
  jq -cs "map(select(.type == \"$1\")) | last" "$SLOG" 2>/dev/null
}

# The assertion helpers.
assert_eq() {
  # $1 actual, $2 expected, $3 label
  if [ "$1" = "$2" ]; then
    ok
  else
    ko "$3: got [$1], want [$2]"
  fi
}
assert_contains() {
  # $1 haystack, $2 needle, $3 label
  if [[ "$1" == *"$2"* ]]; then
    ok
  else
    ko "$3: [$1] does not contain [$2]"
  fi
}
assert_no_context_exhausted() {
  local n
  n=$(count_events context_exhausted)
  assert_eq "$n" 0 "no context_exhausted marker"
}

NEW_WORK() {
  WORK="$ROOT/scratch/e2e-compact/$1"
  rm -rf "$WORK"
  SESSIONS_DIR="$WORK/sessions/session"
  SLOG="$SESSIONS_DIR/events.jsonl"
  SSTATE="$SESSIONS_DIR/compact.json"
  mkdir -p "$SESSIONS_DIR" "$WORK/sessions"
  echo "==== scenario: $1"
}

# The stub plan line: one normal model answer (a final stop).
NORMAL_STOP='{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":100,"output_tokens":10}}'

# ── Scenario 1: the threshold trigger ────────────────────────────
scenario_threshold() {
  NEW_WORK threshold
  COMPACT_ENABLED=true
  work_config
  seed_session 4000 4200
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  run_step
  assert_eq "$(count_events compaction_summary)" 1 "one compaction_summary"
  assert_eq "$(count_events compaction_failed)" 0 "no compaction_failed"
  assert_no_context_exhausted
  assert_eq "$(claim_state)" "idle" "the loop runs to idle"
  local summary
  summary=$(last_event compaction_summary)
  assert_contains "$(last_event compaction_summary)" "the summary of the old region" "the summary body rides the marker"
  # The next projection carries the summary and drops the old region.
  local req
  req=$(cd "$WORK" && "$BIN_DIR/assemble" --session sessions/session --config config.toml 2>/dev/null)
  assert_contains "$req" "the summary of the old region" "the next request carries the summary"
  local n_hit
  n_hit=$(rg -c 'do the task' <<<"$req" 2>/dev/null || true)
  assert_eq "${n_hit:-0}" 0 "the old region is out of the next request"
}

# ── Scenario 2: the overflow error recovery ──────────────────────
scenario_overflow() {
  NEW_WORK overflow
  COMPACT_ENABLED=true
  work_config
  # Below the trigger: the threshold hook is cold. The model call
  # fails with the SGLang overflow shape, then succeeds.
  seed_session 2800 2900 3000
  cat >"$WORK/plan" <<'EOF'
{"text":"","tool_calls":[],"reasoning":[],"stop_reason":"error","usage":null,"detail":"The input (4500 tokens) is longer than the model's context length (3904 tokens)."}
{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":100,"output_tokens":10}}
EOF
  make_stub
  run_step
  assert_eq "$(count_events compaction_summary)" 1 "one compaction_summary"
  assert_contains "$(last_event compaction_summary)" '"reason":"overflow"' "the marker names the overflow trigger"
  assert_eq "$(count_events compaction_failed)" 0 "no compaction_failed"
  assert_no_context_exhausted
  assert_eq "$(claim_state)" "idle" "the loop runs to idle"
}

# ── Scenario 3: the silent overflow ──────────────────────────────
scenario_silent_overflow() {
  NEW_WORK silent-overflow
  COMPACT_ENABLED=true
  work_config
  # The successful call measures over the input budget (3904).
  # The compact runs, the call is not re-run.
  seed_session 4000 4200
  cat >"$WORK/plan" <<'EOF'
{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":5000,"output_tokens":10}}
EOF
  make_stub
  run_step
  # The threshold hook compacted at the trigger. The silent
  # overflow path compacted again at the usage crossing.
  local n_summary
  n_summary=$(count_events compaction_summary)
  if [ "$n_summary" -ge 1 ]; then
    ok
  else
    ko "the silent-overflow compact fired: got no compaction_summary"
  fi
  assert_no_context_exhausted
  assert_eq "$(claim_state)" "idle" "the loop runs to idle"
  local n_assistant
  n_assistant=$(count_events assistant_message)
  # The seed has three assistant turns. The step logs one more.
  assert_eq "$n_assistant" 4 "no model re-run on the silent overflow"
}

# The post-engage variant: the trim state file exists, the
# readings stay below the trigger level, and the successful call
# measures over the input budget. The hook is cold. The silent
# overflow path is the only compact.
scenario_silent_overflow_post_engage() {
  NEW_WORK silent-post-engage
  COMPACT_ENABLED=true
  work_config
  seed_session 2800 2900 3000
  cat >"$SSTATE" <<'EOF'
{"v":2,"caps":{"result":500,"text":200},"keep":24,"drops":0,"engaged_at":3,"last_tokens":3300,"last_at":3,"drops_at_last":0,"per_group":0,"boundary_seq":0}
EOF
  cat >"$WORK/plan" <<'EOF'
{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":5000,"output_tokens":10}}
EOF
  make_stub
  run_step
  local n_summary
  n_summary=$(count_events compaction_summary)
  assert_eq "$n_summary" 1 "one compaction_summary on the silent overflow"
  assert_contains "$(last_event compaction_summary)" '"reason":"overflow"' "the marker names the overflow trigger"
  assert_no_context_exhausted
  assert_eq "$(claim_state)" "idle" "the loop runs to idle"
  local n_assistant
  n_assistant=$(count_events assistant_message)
  assert_eq "$n_assistant" 4 "no model re-run on the silent overflow"
}

# ── Scenario 4: the length stop recovery ─────────────────────────
scenario_length_stop() {
  NEW_WORK length-stop
  COMPACT_ENABLED=true
  work_config
  seed_session 2800 2900 3000
  # The length stop with the partial tool call: the group lands in
  # the log, the compact strips it, the retry succeeds.
  cat >"$WORK/plan" <<'EOF'
{"text":"","tool_calls":[{"id":"c9","name":"bash","arguments":{"command":"echo a truncated argument that the provider cut off mid-call"}}],"reasoning":[],"stop_reason":"length","usage":{"input_tokens":3800,"output_tokens":10}}
{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":100,"output_tokens":10}}
EOF
  make_stub
  run_step
  assert_eq "$(count_events compaction_summary)" 1 "one compaction_summary"
  assert_contains "$(last_event compaction_summary)" '"reason":"overflow"' "the marker names the overflow trigger"
  # The truncated group landed in the log: the truncation notice
  # result is there.
  assert_contains "$(cat "$SLOG")" "Arguments may be truncated" "the truncation notice result landed in the log"
  assert_no_context_exhausted
  local n_err
  n_err=$(jq -c 'select(.type == "error")' "$SLOG" | wc -l)
  assert_eq "$n_err" 0 "no terminal error event"
  assert_eq "$(claim_state)" "idle" "the loop runs to idle"
  # The re-included request drops the truncated pair: the two-
  # prefix rule.
  local req
  req=$(cd "$WORK" && "$BIN_DIR/assemble" --session sessions/session --config config.toml 2>/dev/null)
  local n_hit
  n_hit=$(rg -c 'Arguments may be truncated' <<<"$req" 2>/dev/null || true)
  assert_eq "${n_hit:-0}" 0 "the re-included request drops the truncated pair"
}

# ── Scenario 5: the compact failure ──────────────────────────────
scenario_compact_failure() {
  NEW_WORK compact-failure
  COMPACT_ENABLED=true
  work_config
  seed_session 4000 4200
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  STUB_SUMMARY_FAILS=1
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml" MODEL_BIN="$WORK/stub-model"
    export STUB_PLAN="$WORK/plan" STUB_STATE="$WORK/stub-n" STUB_SUMMARY_FAILS=1
    "$SCRIPT_DIR/step.sh" session 2>/dev/null || true
  )
  assert_eq "$(count_events compaction_failed)" 1 "the compaction_failed marker"
  assert_eq "$(count_events compaction_summary)" 0 "no compaction_summary"
  local n_err
  n_err=$(jq -c 'select(.type == "error")' "$SLOG" | wc -l)
  assert_eq "$n_err" 0 "no terminal event"
  assert_eq "$(claim_state)" "idle" "the loop continues to idle"
}

# ── Scenario 6: the empty summary ────────────────────────────────
scenario_empty_summary() {
  NEW_WORK empty-summary
  COMPACT_ENABLED=true
  work_config
  seed_session 4000 4200
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml" MODEL_BIN="$WORK/stub-model"
    export STUB_PLAN="$WORK/plan" STUB_STATE="$WORK/stub-n" STUB_SUMMARY_EMPTY=1
    "$SCRIPT_DIR/step.sh" session 2>/dev/null || true
  )
  assert_eq "$(count_events compaction_failed)" 1 "the compaction_failed marker"
  assert_eq "$(count_events compaction_summary)" 0 "no compaction_summary"
  assert_eq "$(claim_state)" "idle" "the loop continues to idle"
}

# ── Scenario 7: the iterative merge ─────────────────────────────
scenario_iterative() {
  NEW_WORK iterative
  COMPACT_ENABLED=true
  work_config
  seed_session 4000 4200
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  run_step
  # The second trigger level: the session grows past the new
  # boundary, the second compact carries the first summary. The
  # batch ends on a user turn so the claim is awaiting_model.
  local A
  A="$(printf 'd%.0s' {1..4000})$(printf 'd1%.0s' {1..4000})"
  cat >>"$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t14","seq":14,"content":"more work"}
{"v":1,"type":"assistant_message","ts":"t15","seq":15,"content":"step four","reasoning":[],"tool_calls":[{"id":"c4","name":"bash","arguments":{"command":"ls"}}],"usage":{"input_tokens":5000,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t16","seq":16,"id":"c4","value":{"text":"$A"},"is_error":false}
{"v":1,"type":"stop","ts":"t17","seq":17,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t18","seq":18,"content":"keep going"}
EOF
  echo "$NORMAL_STOP" >"$WORK/plan"
  run_step
  local n_summary
  n_summary=$(count_events compaction_summary)
  assert_eq "$n_summary" 2 "two compaction_summaries in the session"
  # The second summary request carried the first summary: the
  # reqlog holds the summary call requests in order.
  local reqlog="$WORK/reqlog"
  if [ -s "$reqlog" ]; then
    local second
    second=$(sed -n '2p' "$reqlog")
    assert_contains "$second" "the summary of the old region" "the second summary request carries the first summary"
  else
    ko "the reqlog is empty: the iterative merge was not exercised"
  fi
}

# ── Scenario 8: the last-resort compaction ───────────────────────
scenario_last_resort() {
  NEW_WORK last-resort
  # The threshold hook off: the form must escalate on the seed's
  # readings alone. The last-resort path is not gated by the switch.
  COMPACT_ENABLED=false
  work_config
  seed_session 4000 4200
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  # The state file parks the compact form at the floor: the keep
  # window is two, the drops hold the max (the two groups before the
  # floor tail), and the last reading outgrows the budget. A natural
  # engagement lands at the end of the log, leaves no reading at or
  # after it, and the form cannot escalate. The hand-set state is
  # the last-resort fixture: the next assemble run exhausts.
  jq -cn '{v:2,boundary_seq:0,caps:{result:500,text:200},drops:2,drops_at_last:0,engaged_at:0,keep:2,last_tokens:4300,last_at:7,per_group:0}' > "$SSTATE"
  # The keep knob of the work config: with the engaged trim caps
  # (result 500, text 200) the seed estimates about 410 tokens, so
  # the default 2000 keep window covers the log and the forced
  # compact no-ops. 200 keeps two results and cuts the first group:
  # the old region is non-empty.
  sed -i 's/^compact_keep_tokens = 2000$/compact_keep_tokens = 200/' "$WORK/config.toml"
  # Run assemble until the form exhausts: the keep window halves to
  # two, the drops reach the max, and the prediction still outgrows
  # the budget.
  local i
  for i in 1 2 3 4 5 6 7 8; do
    (
      cd "$WORK"
      CONFIG="$WORK/config.toml" "$BIN_DIR/assemble" --session sessions/session --config config.toml > /dev/null 2>&1 || true
    )
  done
  # The Exhausted form is live: the request type is
  # context_exhausted.
  local form
  form=$(
    cd "$WORK"
    CONFIG="$WORK/config.toml" "$BIN_DIR/assemble" --session sessions/session --config config.toml 2>/dev/null | jq -r .type
  )
  if [ "$form" != "context_exhausted" ]; then
    ko "the assemble form did not exhaust (form: $form): the last-resort case was not exercised"
    return
  fi
  run_step
  assert_eq "$(count_events compaction_summary)" 1 "one compaction_summary"
  assert_no_context_exhausted
  # No new session: the sessions root holds only the work session.
  local sessions
  sessions=$(ls "$WORK/sessions" 2>/dev/null | grep -v '^session$' | wc -l)
  assert_eq "$sessions" 0 "no new session directory"
  assert_eq "$(claim_state)" "idle" "the loop runs to idle in the original session"
}

# ── Scenario 9: the failed retry ────────────────────────────────
scenario_failed_retry() {
  NEW_WORK failed-retry
  COMPACT_ENABLED=true
  work_config
  seed_session 2800 2900 3000
  # Every normal call overflows. The recovery compact fails on its
  # two summary calls (the fail-2 counter), the last-resort forced
  # compact succeeds, the retry overflows again, and the terminal
  # error stops the loop in the original session.
  local OVFEVENT
  OVFEVENT='{"text":"","tool_calls":[],"reasoning":[],"stop_reason":"error","usage":null,"detail":"exceeds the context window"}'
  cat >"$WORK/plan" <<EOF
$OVFEVENT
$OVFEVENT
$OVFEVENT
EOF
  echo 0 >"$WORK/summary-fail-count"
  export STUB_SUMMARY_FAIL_COUNT_FILE="$WORK/summary-fail-count" STUB_SUMMARY_FAIL_N=2
  make_stub
  run_step
  local n_summary n_failed
  n_summary=$(count_events compaction_summary)
  n_failed=$(count_events compaction_failed)
  if [ "$n_summary" -ge 1 ] && [ "$n_failed" -ge 1 ]; then
    ok
  else
    ko "the failed retry: got $n_summary summaries and $n_failed failed markers, want at least one of each"
  fi
  local last_err
  last_err=$(jq -cs 'map(select(.type == "error")) | last' "$SLOG" 2>/dev/null)
  assert_contains "$last_err" "last-resort" "the terminal event names the last-resort failure"
  assert_no_context_exhausted
  assert_eq "$(claim_state)" "idle" "the loop stops in the original session"
}

# The kill switch: compact_enabled off sends the overflow path to
# the last-resort compaction without the overflow compaction.
scenario_kill_switch() {
  NEW_WORK kill-switch
  COMPACT_ENABLED=false
  work_config
  seed_session 2800 2900 3000
  local OVFEVENT
  OVFEVENT='{"text":"","tool_calls":[],"reasoning":[],"stop_reason":"error","usage":null,"detail":"exceeds the context window"}'
  cat >"$WORK/plan" <<EOF
$OVFEVENT
$OVFEVENT
EOF
  make_stub
  run_step
  local n_summary
  n_summary=$(count_events compaction_summary)
  if [ "$n_summary" -ge 1 ]; then
    ok
  else
    ko "the last-resort compact ran on the kill switch: got no compaction_summary"
  fi
  assert_no_context_exhausted
}

# The no-detail case: an error stop with no detail takes the
# transport path. No compact marker.
scenario_no_detail() {
  NEW_WORK no-detail
  COMPACT_ENABLED=true
  work_config
  seed_session 2800 2900 3000
  # The error stop without a detail: the transport path. Two
  # retries, both fail: the terminal event stops the loop. No
  # compact marker lands.
  local EVENT
  EVENT='{"text":"","tool_calls":[],"reasoning":[],"stop_reason":"error","usage":null}'
  cat >"$WORK/plan" <<EOF
$EVENT
$EVENT
$EVENT
EOF
  make_stub
  run_step
  assert_eq "$(count_events compaction_summary)" 0 "no compact on the no-detail error"
  assert_eq "$(count_events compaction_failed)" 0 "no compaction_failed"
  local last_err
  last_err=$(jq -cs 'map(select(.type == "error")) | last | .message // ""' "$SLOG" 2>/dev/null)
  assert_contains "$last_err" "model API call failed" "the terminal event is the transport failure"
}

# The scenario filter: run one scenario by name.
run_scenario() {
  local name="$1"
  shift
  if [[ -n "${E2E_ONLY:-}" && "$E2E_ONLY" != "$name" ]]; then
    return 0
  fi
  "$@"
}

# ── Run ──────────────────────────────────────────────────────────
run_scenario threshold scenario_threshold
run_scenario overflow scenario_overflow
run_scenario silent-overflow scenario_silent_overflow
run_scenario silent-post-engage scenario_silent_overflow_post_engage
run_scenario length-stop scenario_length_stop
run_scenario compact-failure scenario_compact_failure
run_scenario empty-summary scenario_empty_summary
run_scenario iterative scenario_iterative
run_scenario last-resort scenario_last_resort
run_scenario failed-retry scenario_failed_retry
run_scenario kill-switch scenario_kill_switch
run_scenario no-detail scenario_no_detail

echo
echo "compact e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
