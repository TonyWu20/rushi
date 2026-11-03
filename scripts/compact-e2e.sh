#!/usr/bin/env bash
# The session-engine compact e2e suite (docs FT-038, design P7).
#
# The one-shot compact mechanism: the compact binary reads the log,
# calls the model with no tools, and runs a bounded check loop (the
# format gate and the content gate) over up to
# `compact_max_summary_calls` responses. It writes the versioned
# handoff file for its target and appends the marker. The loop keeps
# the session: no new session directory.
#
# This suite drives `rushi step` with a stub model. The stub answers
# the loop's normal model calls from the STUB_PLAN lines in order. For
# the compact one-shot call (the request carries the no-tools notice),
# the stub returns a header-laden summary so the content gate passes.
# The assertions prove the marker shapes, the versioned files, and the
# version links.

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

# ── The stub model: the normal turns and the compact calls ───────
# The stub answers the loop's normal model calls from the STUB_PLAN
# lines in order (the STUB_STATE counter). The compact one-shot call
# carries the no-tools notice in its request (docs FT-038 component
# C). For that call the stub returns a header-laden summary so the
# content gate passes. With COMPACT_FAIL=1 the stub returns a
# headerless response instead, to exercise the liveness seed fallback.
make_stub() {
  cat > "$WORK/stub-model" <<'EOF'
#!/usr/bin/env bash
set -u
if [[ "${1:-}" == "--describe" ]]; then
  echo '{"active":"stub","model_id":"stub-model","reasoning_effort":"none","thinking_level":"off"}'
  exit 0
fi
req=$(cat)
if [[ "$req" == *"No tools are available in this session"* ]]; then
  if [[ "${COMPACT_FAIL:-0}" == "1" ]]; then
    echo '{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":100,"output_tokens":10}}'
  else
    echo '{"text":"## Goal\nsummarize the old region\n## Constraints & Preferences\nnone\n## Progress\n### Done\nsummarized the old region\n### In Progress\nnone\n### Blocked\nnone\n## Key Decisions\nkeep the walk-back bound\n## Next Steps\n1. continue\n## Critical Context\nthe summary of the old region","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":100,"output_tokens":60}}'
  fi
  exit 0
fi
N=$(cat "${STUB_STATE:-}" 2>/dev/null || echo 0)
N=$((N + 1))
echo "$N" > "${STUB_STATE:-/dev/null}"
line=$(sed -n "${N}p" "${STUB_PLAN:-/dev/null}")
[[ -z "$line" ]] && line=$(tail -1 "${STUB_PLAN:-/dev/null}")
echo "$line"
exit 0
EOF
  chmod +x "$WORK/stub-model"
}

# ── The work config ──────────────────────────────────────────────
# window 8000, output 4096, trigger 7500 (context_budget - reserve).
# The session engine is the one compact mechanism: there is no
# engine config key.
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
compact_reserve_tokens = 500
compact_keep_tokens = 2000
compact_enabled = $COMPACT_ENABLED

[system_prompt]
text = "test"
EOF
}

# ── The session builder ──────────────────────────────────────────
# A session that crosses the trigger level: three assistant turns,
# the usage readings above the trigger. The results carry 8000 chars
# each (2000 tokens) so the keep window leaves a non-empty old region.
seed_session() {
  # $1 = the first usage reading, $2 = the second, $3 = the third,
  # $4 = the tool result half-size in chars (default 4000).
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

# Run one step against the work session with the stub model.
run_step() {
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_PLAN="$WORK/plan" STUB_STATE="$WORK/stub-n"
    export COMPACT_FAIL="${COMPACT_FAIL:-0}"
    "$BIN_DIR/rushi" step session 2>/dev/null
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
  n=$(jq -c "select(.type == \"$1\")" <(sl <(sl "$SLOG")) 2>/dev/null | wc -l)
  echo "$n"
}

# The last marker of a type.
last_event() {
  jq -cs "map(select(.type == \"$1\")) | last" <(sl <(sl "$SLOG")) 2>/dev/null
}

# The assertion helpers.
assert_eq() {
  if [ "$1" = "$2" ]; then
    ok
  else
    ko "$3: got [$1], want [$2]"
  fi
}
assert_contains() {
  if [[ "$1" == *"$2"* ]]; then
    ok
  else
    ko "$3"
  fi
}

# ── The work dir ────────────────────────────────────────────────
NEW_WORK() {
  WORK="$ROOT/scratch/e2e-compact/$1"
  rm -rf "$WORK"
  SESSIONS_DIR="$WORK/sessions/session"
  SLOG="$SESSIONS_DIR/events.jsonl"
  sl() {
    if head -c 4 "$SLOG" 2>/dev/null | od -An -v -tx1 | head -1 | rg -q '28 b5 2f fd'; then
      zstd -dc "$SLOG" 2>/dev/null
    else
      cat "$SLOG"
    fi
  }
  mkdir -p "$SESSIONS_DIR" "$WORK/sessions"
  echo "==== scenario: $1"
}

# The stub plan line: one normal model answer (a final stop).
NORMAL_STOP='{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":100,"output_tokens":10}}'

# The compact one-shot summary carries this line; the marker and the
# versioned file hold it when the check loop accepts the response.
SUMMARY_MARK="the summary of the old region"

# The liveness fallback seed is the bounded transcript. The task
# statement (the first user message) never drops, so it rides it.
SEED_MARK="the old work"

# ── Scenario 1: the threshold trigger ────────────────────────────
# The proactive compact fires before the model call. The one-shot
# check loop accepts the header-laden stub summary; the marker rides
# that summary; the versioned handoff file is written.
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
  assert_eq "$(claim_state)" "idle" "the loop runs to idle"
  # P3: the versioned handoff file exists and matches the latest copy.
  assert_eq "$(test -f "$SESSIONS_DIR/handoff/v1.md" && echo yes)" "yes" "handoff/v1.md exists"
  assert_eq "$(test -f "$SESSIONS_DIR/handoff.md" && echo yes)" "yes" "handoff.md exists"
  if diff -q "$SESSIONS_DIR/handoff/v1.md" "$SESSIONS_DIR/handoff.md" >/dev/null 2>&1; then
    ok "handoff.md matches handoff/v1.md"
  else
    ko "handoff.md does not match handoff/v1.md"
  fi
  # The marker carries the accepted one-shot summary.
  assert_contains "$(last_event compaction_summary)" "$SUMMARY_MARK" "the summary body rides the marker"
}

# ── Scenario 2: the second compact carries the first ─────────────
scenario_iterative() {
  NEW_WORK iterative
  COMPACT_ENABLED=true
  work_config
  seed_session 4000 4200 4300 6000
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  run_step
  # The session grows past the new boundary; the second compact fires.
  local A
  A="$(printf 'd%.0s' $(seq 1 24000))"
  cat >>"$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t14","seq":14,"content":"more work"}
{"v":1,"type":"assistant_message","ts":"t15","seq":15,"content":"step four","reasoning":[],"tool_calls":[{"id":"c4","name":"bash","arguments":{"command":"ls"}}],"usage":{"input_tokens":5000,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t16","seq":16,"id":"c4","value":{"text":"$A"},"is_error":false}
{"v":1,"type":"stop","ts":"t17","seq":17,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t18","seq":18,"content":"keep going"}
EOF
  run_step
  assert_eq "$(count_events compaction_summary)" 2 "two compaction_summaries in the session"
  # P2: the second marker links to the first.
  local m2
  m2=$(last_event compaction_summary)
  assert_eq "$(jq -r '.version' <<<"$m2")" 2 "the second marker is version 2"
  assert_eq "$(jq -r '.parent_version' <<<"$m2")" 1 "the parent link is version 1"
  # P3: two versioned handoff files; handoff.md matches the latest.
  assert_eq "$(test -f "$SESSIONS_DIR/handoff/v1.md" && echo yes)" "yes" "handoff/v1.md exists"
  assert_eq "$(test -f "$SESSIONS_DIR/handoff/v2.md" && echo yes)" "yes" "handoff/v2.md exists"
  if diff -q "$SESSIONS_DIR/handoff/v2.md" "$SESSIONS_DIR/handoff.md" >/dev/null 2>&1; then
    ok "handoff.md matches handoff/v2.md"
  else
    ko "handoff.md does not match handoff/v2.md"
  fi
}

# ── Scenario 3: the overflow trigger ─────────────────────────────
# Below the threshold: the threshold hook is cold. The model call
# fails with the overflow shape, then the overflow compact fires,
# then the retry succeeds. The results carry 5600 chars each (1400
# tokens) so the walk-back old region is non-empty; the usage readings
# stay under the trigger.
scenario_overflow() {
  NEW_WORK overflow
  COMPACT_ENABLED=true
  work_config
  seed_session 2800 2900 3000 2800
  cat >"$WORK/plan" <<'EOF'
{"text":"","tool_calls":[],"reasoning":[],"stop_reason":"error","usage":null,"detail":"The input (4500 tokens) is longer than the model's context length (3904 tokens)."}
{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":100,"output_tokens":10}}
EOF
  make_stub
  run_step
  assert_eq "$(count_events compaction_summary)" 1 "one compaction_summary"
  assert_contains "$(last_event compaction_summary)" '"reason":"overflow"' "the overflow reason rides the marker"
  assert_eq "$(test -f "$SESSIONS_DIR/handoff/v1.md" && echo yes)" "yes" "handoff/v1.md exists"
  assert_eq "$(claim_state)" "idle" "the loop runs to idle"
}

# ── Scenario 4: the branch compact ───────────────────────────────
# The `--branch` run does a one-shot call over the ditched span. The
# one-shot writes the versioned branch summary file, not a handoff
# file. The marker is an add-on: it compacts nothing.
scenario_branch() {
  NEW_WORK branch
  COMPACT_ENABLED=true
  work_config
  # Session layout (docs/branch-summarize-cases.md Phase 0):
  #   seq 1-4:  initial work
  #   seq 5-8:  branch A
  #   seq 9:    rewind to seq 3 (mode=on), abandoning seqs 4..9
  #   seq 10-13: re-entered active path
  cat > "$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"do the task"}
{"v":1,"type":"assistant_message","ts":"t2","seq":2,"content":"step one","reasoning":[],"tool_calls":[{"id":"c1","name":"read","arguments":{"file_path":"a.txt"}}],"usage":{"input_tokens":2000,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t3","seq":3,"id":"c1","value":{"text":"branch work notes, step one"},"is_error":false}
{"v":1,"type":"stop","ts":"t4","seq":4,"stop_reason":"end_turn"}
{"v":1,"type":"user_message","ts":"t5","seq":5,"content":"continue"}
{"v":1,"type":"assistant_message","ts":"t6","seq":6,"content":"step two","reasoning":[],"tool_calls":[{"id":"c2","name":"bash","arguments":{"command":"ls"}}],"usage":{"input_tokens":2000,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t7","seq":7,"id":"c2","value":{"text":"branch work notes, step two"},"is_error":false}
{"v":1,"type":"stop","ts":"t8","seq":8,"stop_reason":"end_turn"}
{"v":1,"type":"rewind","ts":"t9","seq":9,"target_seq":3,"mode":"on"}
{"v":1,"type":"user_message","ts":"t10","seq":10,"content":"try a different approach"}
{"v":1,"type":"assistant_message","ts":"t11","seq":11,"content":"step three","reasoning":[],"tool_calls":[{"id":"c3","name":"bash","arguments":{"command":"ls -l"}}],"usage":{"input_tokens":2000,"output_tokens":50}}
{"v":1,"type":"tool_result","ts":"t12","seq":12,"id":"c3","value":{"text":"the rework"},"is_error":false}
{"v":1,"type":"stop","ts":"t13","seq":13,"stop_reason":"end_turn"}
EOF
  # A pre-existing handoff.md must stay byte-identical.
  echo "PRE-EXISTING HANDOFF SENTINEL" > "$SESSIONS_DIR/handoff.md"
  make_stub
  echo "$NORMAL_STOP" >"$WORK/plan"
  local status
  status=$(
    cd "$WORK"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_PLAN="$WORK/plan" STUB_STATE="$WORK/stub-n"
    "$BIN_DIR/compact" "$SESSIONS_DIR" --config "$WORK/config.toml" --branch 2>/dev/null
  )
  assert_eq "$(jq -r '.status' <<<"$status")" "compacted" "the branch run compacts"
  assert_eq "$(jq -r '.branch' <<<"$status")" "true" "the status flags the branch mode"
  assert_eq "$(jq -r '.branch_of' <<<"$status")" 9 "the marker anchors on the rewind seq"
  local marker
  marker=$(last_event compaction_summary)
  assert_eq "$(jq -r '.branch_of' <<<"$marker")" 9 "the marker carries branch_of"
  assert_eq "$(jq -r '.first_kept_seq' <<<"$marker")" 1 "the first_kept sentinel"
  assert_eq "$(jq -r '.diverge_seq' <<<"$marker")" 9 "diverge_seq is the rewind seq"
  assert_eq "$(jq -r '.version' <<<"$marker")" 1 "the branch namespace starts at 1"
  # The one-shot wrote the versioned branch summary file, not a handoff.
  local file
  file="$SESSIONS_DIR/branch-summary/v1.md"
  assert_eq "$(test -f "$file" && echo yes)" "yes" "the versioned branch summary file exists"
  assert_contains "$(cat "$file")" "$SUMMARY_MARK" "the file holds the summary"
  assert_eq "$(cat "$SESSIONS_DIR/handoff.md")" "PRE-EXISTING HANDOFF SENTINEL" "handoff.md is unchanged"
  assert_eq "$(test -d "$SESSIONS_DIR/handoff" && echo yes)" "" "no handoff/ dir for a branch marker"
}

# ── Scenario 5: the one-shot failure (the liveness fallback) ─────
# The stub returns a headerless response for the compact call, so the
# check loop exhausts its calls. The main target falls back to the
# bounded model-free cut: the seed transcript becomes the handoff. No
# `compaction_failed` marker; a `compaction_summary` rides the
# fallback, and the handoff file is written. The main session is never
# stuck.
scenario_compact_failure() {
  NEW_WORK compact-failure
  COMPACT_ENABLED=true
  work_config
  seed_session 6000 6200 6500
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  COMPACT_FAIL=1 run_step
  # The liveness contract: the fallback finishes the compact. No
  # compaction_failed marker; a compaction_summary rides the fallback.
  assert_eq "$(count_events compaction_failed)" 0 "no compaction_failed (the fallback finishes the compact)"
  assert_eq "$(count_events compaction_summary)" 1 "one compaction_summary (the fallback summary)"
  # The fallback handoff is written with the seed transcript.
  assert_eq "$(test -f "$SESSIONS_DIR/handoff/v1.md" && echo yes)" "yes" "the fallback writes the handoff"
  assert_contains "$(cat "$SESSIONS_DIR/handoff/v1.md")" "$SEED_MARK" "the fallback handoff is the seed transcript"
}

# ── Scenario 6: the kill switch holds the threshold ──────────────
# compact_enabled=false: the threshold trigger is held. No compact.
scenario_kill_switch() {
  NEW_WORK kill-switch
  COMPACT_ENABLED=false
  work_config
  seed_session 6000 6200 6500
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  run_step
  assert_eq "$(count_events compaction_summary)" 0 "no compaction_summary"
  assert_eq "$(count_events compaction_failed)" 0 "no compaction_failed"
}

# ── Scenario 7: the explicit `--up-to` boundary ──────────────────
# The driver places the boundary at the explicit seq. The one-shot
# summarizes the prefix and writes the versioned handoff file.
scenario_up_to() {
  NEW_WORK up-to
  COMPACT_ENABLED=true
  work_config
  seed_session 6000 6200 6500
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  local status
  status=$(
    cd "$WORK"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_PLAN="$WORK/plan" STUB_STATE="$WORK/stub-n"
    "$BIN_DIR/compact" "$SESSIONS_DIR" --config "$WORK/config.toml" --force --up-to 4 2>/dev/null
  )
  assert_eq "$(jq -r '.status' <<<"$status")" "compacted" "the forced compact compacts"
  assert_eq "$(jq -r '.first_kept_seq' <<<"$status")" 5 "the boundary lands at seq 5"
  assert_eq "$(test -f "$SESSIONS_DIR/handoff/v1.md" && echo yes)" "yes" "handoff/v1.md exists"
  assert_contains "$(cat "$SESSIONS_DIR/handoff/v1.md")" "$SUMMARY_MARK" "the file holds the summary"
}

# ── Scenario 8: the real `write-handoff` tool, both modes ────────
# Run the real tool binary (not the stub). It reads the sidecar and
# writes the target file. Handoff mode writes the versioned file plus
# the latest copy. Branch mode writes only the branch summary file.
scenario_tool() {
  NEW_WORK tool
  local cdir="$WORK/agent"
  local target="$WORK/parent"
  mkdir -p "$cdir" "$target"
  # Handoff mode (the default when `mode` is absent).
  cat > "$cdir/handoff-target.json" <<EOF
{"target_dir":"$target","version":1}
EOF
  HARNESS_SESSION_DIR="$cdir" "$BIN_DIR/write-handoff" <<< '{"summary":"the handoff body"}' > /dev/null
  assert_eq "$(cat "$target/handoff/v1.md")" "the handoff body" "handoff mode writes the versioned file"
  assert_eq "$(cat "$target/handoff.md")" "the handoff body" "handoff mode writes the latest copy"
  # Branch mode.
  cat > "$cdir/handoff-target.json" <<EOF
{"target_dir":"$target","version":2,"mode":"branch"}
EOF
  HARNESS_SESSION_DIR="$cdir" "$BIN_DIR/write-handoff" <<< '{"summary":"the branch body"}' > /dev/null
  assert_eq "$(cat "$target/branch-summary/v2.md")" "the branch body" "branch mode writes the branch file"
  assert_eq "$(cat "$target/handoff.md")" "the handoff body" "branch mode leaves handoff.md unchanged"
}

# ── Run ───────────────────────────────────────────────────────────
run_scenario() {
  local name="$1" fn="$2"
  "$fn"
}

run_scenario threshold scenario_threshold
run_scenario iterative scenario_iterative
run_scenario overflow scenario_overflow
run_scenario branch scenario_branch
run_scenario compact-failure scenario_compact_failure
run_scenario kill-switch scenario_kill_switch
run_scenario up-to scenario_up_to
run_scenario tool scenario_tool

echo
echo "compact e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
