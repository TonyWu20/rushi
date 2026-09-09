#!/usr/bin/env bash
# The auto-compact e2e suite (docs/auto-compact-plan.md section 5).
# It drives `rushi step` with a scriptable stub model binary and asserts
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
# window 8000, output 4096, trigger 7500 (context_budget - reserve).
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
  # $1 = the first usage reading, $2 = the second, $3 = the third,
  # $4 = the tool result half-size in chars (default 4000, so an
  # 8000-char = 2000-token result).
  local u1="$1" u2="$2" u3="${3:-4300}" half="${4:-4000}"
  local A B C
  A="$(printf 'a%.0s' $(seq 1 "$half"))$(printf 'a1%.0s' $(seq 1 "$half"))"
  B="$(printf 'b%.0s' $(seq 1 "$half"))$(printf 'b1%.0s' $(seq 1 "$half"))"
  C="$(printf 'c%.0s' $(seq 1 "$half"))$(printf 'c1%.0s' $(seq 1 "$half"))"
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
    if [[ -n "${STEP_DEBUG:-}" ]]; then
      "$BIN_DIR/rushi" step session
    else
      "$BIN_DIR/rushi" step session 2>/dev/null
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
  seed_session 6000 6200 6500
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

# ── Scenario 1b: the trigger fires at the context budget ────────
# The trigger sits at context_budget - reserve (8000 - 500 = 7500).
# The measured readings (3000-3800) plus trailing tool results
# push the estimate past 7500.
scenario_pi_parity() {
  NEW_WORK pi-parity
  COMPACT_ENABLED=true
  work_config
  seed_session 3000 3500 3800 6000
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  run_step
  assert_eq "$(count_events compaction_summary)" 1 "one compaction_summary"
  assert_contains "$(last_event compaction_summary)" '"reason":"threshold"' "the threshold reason rides the marker"
  local tb
  tb=$(jq -cs 'map(select(.type == "compaction_summary")) | last' "$SLOG" | jq -r '.tokens_before')
  if [[ -n "$tb" && "$tb" -ge 7500 ]]; then
    ok
  else
    ko "tokens_before $tb sits at the context-budget threshold (>= 7500)"
  fi
  assert_no_context_exhausted
  assert_eq "$(claim_state)" "idle" "the loop runs to idle"
}

# ── Scenario 1d: pi parity does not exhaust below the trigger ─────
# Under the `context_budget` base the wire budget is the full window
# (8000), not the input-only window (3904). The kept region below is
# ~4050 tokens: above the clamped budget, below the 7500 trigger.
# Before the unclamp, assemble emitted context_exhausted at the
# clamped budget and the loop died. Now the request proceeds and the
# loop runs to idle with no compaction.
scenario_pi_parity_no_exhaust() {
  NEW_WORK pi-parity-no-exhaust
  COMPACT_ENABLED=false
  work_config
  local R
  R="$(printf 'q%.0s' {1..16000})"  # 16000 chars = 4000 tokens
  cat > "$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"do the task"}
{"v":1,"type":"assistant_message","ts":"t2","seq":2,"content":"step one","reasoning":[],"tool_calls":[{"id":"c1","name":"read","arguments":{"file_path":"a.txt"}}],"usage":{"input_tokens":200,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t3","seq":3,"id":"c1","value":{"text":"small result"},"is_error":false}
{"v":1,"type":"stop","ts":"t4","seq":4,"stop_reason":"end_turn"}
{"v":1,"type":"compaction_summary","ts":"t5","seq":5,"summary":"the summary of the old region","first_kept_seq":6,"reason":"threshold","tokens_before":800}
{"v":1,"type":"user_message","ts":"t6","seq":6,"content":"carry on"}
{"v":1,"type":"assistant_message","ts":"t7","seq":7,"content":"step two","reasoning":[],"tool_calls":[{"id":"c2","name":"bash","arguments":{"command":"ls"}}],"usage":{"input_tokens":4500,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t8","seq":8,"id":"c2","value":{"text":"$R"},"is_error":false}
{"v":1,"type":"stop","ts":"t9","seq":9,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t10","seq":10,"content":"new task"}
EOF
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  run_step
  assert_eq "$(count_events compaction_summary)" 1 "only the seed boundary"
  assert_no_context_exhausted
  local n_err
  n_err=$(jq -c 'select(.type == "error")' "$SLOG" | wc -l)
  assert_eq "$n_err" 0 "no terminal error"
  assert_eq "$(claim_state)" "idle" "the loop runs to idle"
}

# ── Scenario 2: the overflow error recovery ──────────────────────
scenario_overflow() {
  NEW_WORK overflow
  COMPACT_ENABLED=true
  work_config
  # Below the trigger (7500): the threshold hook is cold. The model
  # call fails with the SGLang overflow shape, then succeeds.
  seed_session 2800 2900 3000 2000
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
  # The successful call measures over the context budget (8000).
  # The compact runs, the call is not re-run.
  seed_session 4000 4200 4300 2000
  cat >"$WORK/plan" <<'EOF'
{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":8500,"output_tokens":10}}
EOF
  make_stub
  run_step
  # The silent-overflow path fires on the usage reading.
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
# Phase 2: the sticky compact.json state is gone. This scenario now
# tests the Phase 2 equivalent: a session with a prior compaction_summary
# boundary where the post-boundary events still exceed the budget.
# The silent-overflow path fires the overflow compact on the usage reading.
scenario_silent_overflow_post_engage() {
  NEW_WORK silent-post-engage
  COMPACT_ENABLED=true
  work_config
  seed_session 2800 2900 3000 2000
  cat >"$WORK/plan" <<'EOF'
{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":8500,"output_tokens":10}}
EOF
  make_stub
  run_step
  # The seed estimate stays below the 7500 trigger, so the
  # threshold check is cold. But the model reports input_tokens=8500,
  # which exceeds the 8000 context budget: the silent-overflow path
  # fires.
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
  seed_session 2800 2900 3000 2000
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
  seed_session 6000 6200 6500
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  STUB_SUMMARY_FAILS=1
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml" MODEL_BIN="$WORK/stub-model"
    export STUB_PLAN="$WORK/plan" STUB_STATE="$WORK/stub-n" STUB_SUMMARY_FAILS=1
    "$BIN_DIR/rushi" step session 2>/dev/null || true
  )
  # The threshold compact fails (all summary calls fail). No boundary
  # is created, so context_exhausted does not fire. The model call
  # succeeds normally: one compaction_failed marker, no terminal error.
  assert_eq "$(count_events compaction_failed)" 1 "one compaction_failed marker (threshold)"
  assert_eq "$(count_events compaction_summary)" 0 "no compaction_summary"
  local n_err
  n_err=$(jq -c 'select(.type == "error")' "$SLOG" | wc -l)
  assert_eq "$n_err" 0 "no terminal error (model call succeeded)"
  assert_eq "$(claim_state)" "idle" "the loop continues to idle"
}

# ── Scenario 6: the empty summary ────────────────────────────────
scenario_empty_summary() {
  NEW_WORK empty-summary
  COMPACT_ENABLED=true
  work_config
  seed_session 6000 6200 6500
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml" MODEL_BIN="$WORK/stub-model"
    export STUB_PLAN="$WORK/plan" STUB_STATE="$WORK/stub-n" STUB_SUMMARY_EMPTY=1
    "$BIN_DIR/rushi" step session 2>/dev/null || true
  )
  # The threshold compact produces an empty summary (failure). No
  # boundary is created, so context_exhausted does not fire. The
  # model call succeeds normally: one compaction_failed marker, no
  # terminal error.
  assert_eq "$(count_events compaction_failed)" 1 "one compaction_failed marker (threshold)"
  assert_eq "$(count_events compaction_summary)" 0 "no compaction_summary"
  local n_err
  n_err=$(jq -c 'select(.type == "error")' "$SLOG" | wc -l)
  assert_eq "$n_err" 0 "no terminal error (model call succeeded)"
  assert_eq "$(claim_state)" "idle" "the loop continues to idle"
}

# ── Scenario 7: the iterative merge ─────────────────────────────
scenario_iterative() {
  NEW_WORK iterative
  COMPACT_ENABLED=true
  work_config
  seed_session 4000 4200 4300 6000
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  run_step
  # The second trigger level: the session grows past the new
  # boundary, the second compact carries the first summary. The
  # batch ends on a user turn so the claim is awaiting_model.
  local A
  A="$(printf 'd%.0s' $(seq 1 24000))"
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

# ── Scenario 8: no-trim (realistic compact_enabled=true) ────────
# The realistic configuration: compact_enabled=true.  The kept
# region carries a 14000-char result (~3500 tokens) so the estimate
# exceeds the 3404 trigger level.  The proactive threshold compact
# fires before the model call, summarises the old region, and the
# model call succeeds.  No hard_trim marker is emitted because the
# hard-trim backstop is disabled; the full context is kept so the
# server prompt cache stays warm.
scenario_no_trim() {
  NEW_WORK no-trim
  # A compaction_summary boundary already exists (first_kept_seq 6).
  # The kept region carries an 8000-char and a 28000-char result:
  # the estimate exceeds the 7500 trigger level, so the threshold
  # compact fires before the model call.  With the hard-trim backstop
  # disabled, no group is dropped and no hard_trim marker is logged.
  COMPACT_ENABLED=true
  work_config
  local OLD_D NEW_D
  OLD_D="$(printf 'o%.0s' {1..8000})"
  NEW_D="$(printf 'n%.0s' {1..28000})"
  cat > "$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"do the task"}
{"v":1,"type":"assistant_message","ts":"t2","seq":2,"content":"step one","reasoning":[],"tool_calls":[{"id":"c1","name":"read","arguments":{"file_path":"a.txt"}}],"usage":{"input_tokens":200,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t3","seq":3,"id":"c1","value":{"text":"small result"},"is_error":false}
{"v":1,"type":"stop","ts":"t4","seq":4,"stop_reason":"end_turn"}
{"v":1,"type":"compaction_summary","ts":"t5","seq":5,"summary":"the summary of the old region","first_kept_seq":6,"reason":"threshold","tokens_before":800}
{"v":1,"type":"user_message","ts":"t6","seq":6,"content":"carry on"}
{"v":1,"type":"assistant_message","ts":"t7","seq":7,"content":"step two","reasoning":[],"tool_calls":[{"id":"c2","name":"bash","arguments":{"command":"ls"}}],"usage":{"input_tokens":3800,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t8","seq":8,"id":"c2","value":{"text":"$OLD_D"},"is_error":false}
{"v":1,"type":"stop","ts":"t9","seq":9,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t10","seq":10,"content":"new task"}
{"v":1,"type":"assistant_message","ts":"t11","seq":11,"content":"step three","reasoning":[],"tool_calls":[{"id":"c3","name":"bash","arguments":{"command":"ls"}}],"usage":{"input_tokens":3800,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t12","seq":12,"id":"c3","value":{"text":"$NEW_D"},"is_error":false}
{"v":1,"type":"stop","ts":"t13","seq":13,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t14","seq":14,"content":"continue"}
EOF
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  run_step
  # The threshold compact handled the oversized context: one new
  # compaction_summary on top of the seed boundary.  No hard_trim
  # marker because the hard-trim backstop is disabled.
  assert_eq "$(count_events compaction_summary)" 2 "seed boundary + threshold compact"
  assert_contains "$(last_event compaction_summary)" '"reason":"threshold"' "the new marker names the threshold trigger"
  local n_trim
  n_trim=$(jq -c 'select(.type == "ext_status" and .id == "hard_trim")' "$SLOG" 2>/dev/null | wc -l)
  assert_eq "$n_trim" 0 "no hard_trim marker (trim disabled)"
  assert_no_context_exhausted
  local n_err
  n_err=$(jq -c 'select(.type == "error")' "$SLOG" | wc -l)
  assert_eq "$n_err" 0 "no terminal error"
  assert_eq "$(claim_state)" "idle" "the loop runs to idle in the original session"
}

# The framing (boundary summary) is so large that even the framing
# alone exceeds the target.  The context_exhausted form fires and
# the last-resort forced compact takes over.  compact_enabled=true
# is the realistic setting; the exhausted path is not gated by it.
scenario_context_exhausted() {
  NEW_WORK context-exhausted
  COMPACT_ENABLED=true
  work_config
  local MID_D BIG_SUMMARY
  MID_D="$(printf 'm%.0s' {1..8000})"
  BIG_SUMMARY="$(printf 's%.0s' {1..40000})"
  cat > "$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"do the task"}
{"v":1,"type":"assistant_message","ts":"t2","seq":2,"content":"step one","reasoning":[],"tool_calls":[{"id":"c1","name":"read","arguments":{"file_path":"a.txt"}}],"usage":{"input_tokens":200,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t3","seq":3,"id":"c1","value":{"text":"small result"},"is_error":false}
{"v":1,"type":"stop","ts":"t4","seq":4,"stop_reason":"end_turn"}
{"v":1,"type":"compaction_summary","ts":"t5","seq":5,"summary":"$BIG_SUMMARY","first_kept_seq":6,"reason":"threshold","tokens_before":800}
{"v":1,"type":"user_message","ts":"t6","seq":6,"content":"carry on"}
{"v":1,"type":"assistant_message","ts":"t7","seq":7,"content":"step two","reasoning":[],"tool_calls":[{"id":"c2","name":"bash","arguments":{"command":"ls"}}],"usage":{"input_tokens":500,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t8","seq":8,"id":"c2","value":{"text":"$MID_D"},"is_error":false}
{"v":1,"type":"stop","ts":"t9","seq":9,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t10","seq":10,"content":"new task"}
{"v":1,"type":"assistant_message","ts":"t11","seq":11,"content":"step three","reasoning":[],"tool_calls":[{"id":"c3","name":"bash","arguments":{"command":"ls"}}],"usage":{"input_tokens":500,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t12","seq":12,"id":"c3","value":{"text":"$MID_D"},"is_error":false}
{"v":1,"type":"stop","ts":"t13","seq":13,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t14","seq":14,"content":"another task"}
{"v":1,"type":"assistant_message","ts":"t15","seq":15,"content":"step four","reasoning":[],"tool_calls":[{"id":"c4","name":"bash","arguments":{"command":"ls"}}],"usage":{"input_tokens":500,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t16","seq":16,"id":"c4","value":{"text":"$MID_D"},"is_error":false}
{"v":1,"type":"stop","ts":"t17","seq":17,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t18","seq":18,"content":"continue"}
EOF
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  run_step
  # The context_exhausted form fired: the forced compact ran and
  # added a second compaction_summary.  No hard_trim marker because
  # the hard-trim backstop is disabled.
  assert_eq "$(count_events compaction_summary)" 2 "two compaction_summaries (seed + forced)"
  local n_trim
  n_trim=$(jq -c 'select(.type == "ext_status" and .id == "hard_trim")' "$SLOG" 2>/dev/null | wc -l)
  assert_eq "$n_trim" 0 "no hard_trim marker (trim disabled; framing too large for context_exhausted)"
  local n_err
  n_err=$(jq -c 'select(.type == "error")' "$SLOG" | wc -l)
  assert_eq "$n_err" 0 "no terminal error"
  assert_eq "$(claim_state)" "idle" "the loop recovers to idle after the forced compact"
}

# ── Scenario 9: the failed retry ────────────────────────────────
scenario_failed_retry() {
  NEW_WORK failed-retry
  COMPACT_ENABLED=true
  work_config
  seed_session 2800 2900 3000 2000
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
  seed_session 2800 2900 3000 2000
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
  seed_session 2800 2900 3000 2000
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

# ── Scenario 11b: fork-then-compact ───────────────────────────────
# A session with a rewind fork. The abandoned branch is masked.
# The active context exceeds the trigger level, so compact fires.
# The assembled request after compact must NOT contain the masked
# branch content, and MUST contain the summary.
scenario_fork_compact() {
  NEW_WORK fork-compact
  COMPACT_ENABLED=true
  work_config

  # Session layout:
  #   seq 1-3: initial work (user, assistant+tool_call, tool_result)
  #   seq 4: stop
  #   seq 5-8: branch A (user, assistant+tool_call, tool_result, stop)
  #   seq 9: rewind to seq 3 (mode=on), masking seqs 4-8
  #   seq 10-13: branch B (user, assistant+tool_call, tool_result, stop)
  #   seq 14: user "next"
  #
  # Active path: [1,2,3, 10,11,12,13,14]
  # Masked: [4,5,6,7,8]
  local C_MASKED C_ACTIVE
  C_MASKED="$(printf 'MASKED%.0s' $(seq 1 1000))"   # 8000 chars
  C_ACTIVE="$(printf 'ACTIVE%.0s' $(seq 1 30000))"   # 240000 chars

  cat > "$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"do the task"}
{"v":1,"type":"assistant_message","ts":"t2","seq":2,"content":"step one","reasoning":[],"tool_calls":[{"id":"c1","name":"read","arguments":{"file_path":"a.txt"}}],"usage":{"input_tokens":2000,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t3","seq":3,"id":"c1","value":{"text":"$C_MASKED"},"is_error":false}
{"v":1,"type":"stop","ts":"t4","seq":4,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t5","seq":5,"content":"continue"}
{"v":1,"type":"assistant_message","ts":"t6","seq":6,"content":"step two","reasoning":[],"tool_calls":[{"id":"c2","name":"bash","arguments":{"command":"ls"}}],"usage":{"input_tokens":2000,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t7","seq":7,"id":"c2","value":{"text":"$C_MASKED"},"is_error":false}
{"v":1,"type":"stop","ts":"t8","seq":8,"stop_reason":"end_turn"}
{"v":1,"type":"rewind","ts":"t9","seq":9,"target_seq":3,"mode":"on"}
{"v":1,"type":"user_message","ts":"t10","seq":10,"content":"try different approach"}
{"v":1,"type":"assistant_message","ts":"t11","seq":11,"content":"step three","reasoning":[],"tool_calls":[{"id":"c3","name":"bash","arguments":{"command":"ls"}}],"usage":{"input_tokens":2000,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t12","seq":12,"id":"c3","value":{"text":"$C_ACTIVE"},"is_error":false}
{"v":1,"type":"stop","ts":"t13","seq":13,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t14","seq":14,"content":"next"}
EOF

  echo "$NORMAL_STOP" > "$WORK/plan"
  make_stub
  run_step

  # Compact should have fired (active context exceeds 7500 trigger).
  local n_summary
  n_summary=$(count_events compaction_summary)
  assert_eq "$n_summary" 1 "one compaction_summary after fork-compact"
  assert_no_context_exhausted
  assert_eq "$(claim_state)" "idle" "the loop runs to idle"

  # The next assembled request must carry the summary, not the
  # masked branch content.
  local req
  req=$(cd "$WORK" && "$BIN_DIR/assemble" --session sessions/session --config config.toml 2>/dev/null)
  assert_contains "$req" "the summary of the old region" "the next request carries the summary"
  if echo "$req" | rg -q 'MASKED'; then
    ko "masked branch content leaked into the next request"
  else
    ok "no masked branch content in the next request"
  fi
}

# ── Scenario 12: real-fixture pressure test ─────────────────────
# Uses the real select-and-yank-impl session log (~2800 events,
# ~1.8 MB, last measured input_tokens ≈ 167 k) to verify that the
# compact mechanism can summarise a large real log without the
# summary call itself overflowing the model window.
scenario_real_fixture_pressure() {
  NEW_WORK real-fixture-pressure
  local fixture="$(dirname "$ROOT")/rushi-tui/sessions/select-and-yank-impl/events.jsonl"
  if [[ ! -f "$fixture" ]]; then
    echo "SKIP: fixture not found at $fixture"
    return 0
  fi
  cp "$fixture" "$SLOG"
  # The fixture ends with a user_message + cancel. Append a fresh
  # user message so the step has a pending user turn to process.
  local nlines
  nlines=$(wc -l < "$SLOG")
  local next_seq=$((nlines + 1))
  echo "{\"v\":1,\"type\":\"user_message\",\"ts\":\"t_pressure\",\"seq\":$next_seq,\"content\":\"continue\"}" >> "$SLOG"

  # Use the user's real config: the trigger sits at the context
  # budget minus the reserve:
  #   trigger = context_budget_tokens - compact_reserve_tokens
  #           = 262144 - 16384 = 245760 (pi-parity).
  # The fixture's last measured input is ~167k, below 245760, so we
  # append a large tool_result to push the full-form estimate past
  # the trigger level.
  cat > "$WORK/config.toml" <<EOF
[model]
api = "responses"
max_output_tokens = 32768

[model.stub]
model_id = "stub-model"
base_url = "http://127.0.0.1:1"
api_key_env = "DUMMY"
context_tokens = 262144

[active]
model = "stub"

[paths]
sessions_root = "sessions"

[limits]
context_budget_tokens = 262144
compact_reserve_tokens = 16384
compact_keep_tokens = 20000
compact_enabled = true

[system_prompt]
text = "test"
EOF

  # Append a large tool_result (~80k tokens) so the full-form
  # estimate of the kept region exceeds the 245760 trigger.
  local bigtext
  bigtext=$(printf 'Z%.0s' $(seq 1 320000))
  echo "{\"v\":1,\"type\":\"tool_result\",\"ts\":\"t_pressure2\",\"id\":\"c_big\",\"seq\":$((next_seq+1)),\"value\":{\"text\":\"$bigtext\"},\"is_error\":false}" >> "$SLOG"
  echo "{\"v\":1,\"type\":\"stop\",\"ts\":\"t_pressure3\",\"seq\":$((next_seq+2)),\"stop_reason\":\"end_turn\"}" >> "$SLOG"

  echo "$NORMAL_STOP" > "$WORK/plan"
  make_stub
  run_step

  # The fixture already carries one compaction_summary; a new one
  # must appear from this step's threshold compact.
  local n_summary
  n_summary=$(count_events compaction_summary)
  if [ "$n_summary" -ge 2 ]; then
    ok
  else
    ko "expected ≥2 compaction_summaries (seed + pressure compact), got $n_summary"
  fi
  assert_no_context_exhausted
  local n_err
  n_err=$(jq -c 'select(.type == "error")' "$SLOG" | wc -l)
  # The fixture already has 28 pre-existing error events; count only
  # new ones beyond the fixture baseline.
  local fixture_errors
  fixture_errors=$(rg -c '"type":"error"' "$fixture" 2>/dev/null || echo 0)
  if [ "$n_err" -le "$fixture_errors" ]; then
    ok
  else
    ko "unexpected new error events: $n_err total, $fixture_errors from fixture"
  fi
  assert_eq "$(claim_state)" "idle" "the loop runs to idle"
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
run_scenario pi-parity scenario_pi_parity
run_scenario pi-parity-no-exhaust scenario_pi_parity_no_exhaust
run_scenario overflow scenario_overflow
run_scenario silent-overflow scenario_silent_overflow
run_scenario silent-post-engage scenario_silent_overflow_post_engage
run_scenario length-stop scenario_length_stop
run_scenario compact-failure scenario_compact_failure
run_scenario empty-summary scenario_empty_summary
run_scenario iterative scenario_iterative
run_scenario no-trim scenario_no_trim
run_scenario context-exhausted scenario_context_exhausted
run_scenario failed-retry scenario_failed_retry
run_scenario kill-switch scenario_kill_switch
run_scenario no-detail scenario_no_detail
run_scenario fork-compact scenario_fork_compact
run_scenario real-fixture-pressure scenario_real_fixture_pressure

echo
echo "compact e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
