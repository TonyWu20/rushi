#!/usr/bin/env bash
# The session-engine compact e2e suite (docs FT-033, design P7).
#
# The one-shot compact mechanism is removed. The compact driver forks
# a `rushi run` compact agent whose only tool is `write-handoff`.
# This suite drives `rushi step` with a stub model for the normal
# loop turns and stubs the compact agent fork (the RUSHI_BIN override)
# so it writes the handoff or branch summary file exactly as the
# agent's tool would. The assertions prove the marker shapes, the
# versioned files, and the version links.
#
# The stub model answers the loop's normal model calls from the
# STUB_PLAN lines in order. The fake `rushi` fork reads the sidecar
# the driver wrote and writes the target file. It never runs a real
# agent loop; it mimics the agent plus the `write-handoff` tool.

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

# ── The stub model: the loop's normal turns ──────────────────────
make_stub() {
  cat > "$WORK/stub-model" <<'EOF'
#!/usr/bin/env bash
set -u
if [[ "${1:-}" == "--describe" ]]; then
  echo '{"active":"stub","model_id":"stub-model","reasoning_effort":"none","thinking_level":"off"}'
  exit 0
fi
cat > /dev/null
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

# ── The fake `rushi` fork: the compact agent + the tool ─────────
# The compact driver forks `rushi run <c_id>
# --sessions-root R --cwd D --config C`, with no task. The seed
# transcript is pre-seeded as the session's first `user_message` in the
# agent log, never an argv slot (the argv limit is the FT-035 failure).
# The fork reads the sidecar and writes the target file, as the
# agent's `write-handoff` tool would. `mode` in the sidecar selects
# the handoff or the branch output. `FAKE_RUSHI_FAIL` and
# `FAKE_RUSHI_SUMMARY` drive the failure and content.
make_fake_rushi() {
  cat > "$WORK/fake-rushi" <<'EOF'
#!/usr/bin/env bash
set -u
c_id=""
R=""
while [ $# -gt 0 ]; do
  case "$1" in
    run) shift ;;
    --sessions-root) R="$2"; shift 2 ;;
    --cwd) shift 2 ;;
    --config) shift 2 ;;
    *) if [ -z "$c_id" ]; then c_id="$1"; fi; shift ;;
  esac
done
if [[ "${FAKE_RUSHI_FAIL:-0}" == "1" ]]; then
  echo "fake rushi: the compact agent session failed" >&2
  exit 1
fi
c_dir="$R/$c_id"
# The driver pre-seeded the seed transcript as the session's first
# user_message before the fork, so the log exists and is non-empty.
log="$c_dir/events.jsonl"
[ -s "$log" ] || { echo "fake rushi: the seed log is missing or empty" >&2; exit 6; }
side="$c_dir/handoff-target.json"
[ -f "$side" ] || { echo "fake rushi: no sidecar" >&2; exit 3; }
target="$(sed -n 's/.*"target_dir"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$side")"
version="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*\([0-9]\+\).*/\1/p' "$side")"
mode="$(sed -n 's/.*"mode"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$side")"
[ -n "$target" ] && [ -n "$version" ] || { echo "fake rushi: bad sidecar" >&2; exit 4; }
summary="${FAKE_RUSHI_SUMMARY:-from the stub compact agent}"
if [[ "$mode" == "branch" ]]; then
  mkdir -p "$target/branch-summary"
  printf '%s\n' "$summary" > "$target/branch-summary/v${version}.md"
else
  mkdir -p "$target/handoff"
  printf '%s\n' "$summary" > "$target/handoff/v${version}.md"
  printf '%s\n' "$summary" > "$target/handoff.md"
fi
exit 0
EOF
  chmod +x "$WORK/fake-rushi"
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

# Run one step against the work session with the stub model and the
# fake compact-agent fork.
run_step() {
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml"
    export MODEL_BIN="$WORK/stub-model"
    export RUSHI_BIN="$WORK/fake-rushi"
    export STUB_PLAN="$WORK/plan" STUB_STATE="$WORK/stub-n"
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

# ── Scenario 1: the threshold trigger ────────────────────────────
# The proactive compact fires before the model call. The driver forks
# the compact agent; the agent writes the versioned handoff file; the
# marker rides the summary.
scenario_threshold() {
  NEW_WORK threshold
  COMPACT_ENABLED=true
  work_config
  seed_session 6000 6200 6500
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  make_fake_rushi
  FAKE_RUSHI_SUMMARY="the summary of the old region" run_step
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
  # The marker carries the agent-written summary.
  assert_contains "$(last_event compaction_summary)" "the summary of the old region" "the summary body rides the marker"
  # The agent session dir was removed after the marker.
  local cdirs
  cdirs=$(rg -l 'compact-v' "$SESSIONS_DIR/.." 2>/dev/null | wc -l)
  assert_eq "$(find "$WORK/sessions" -maxdepth 1 -name 'session-compact-*' | wc -l)" 0 "the agent session dir was removed"
}

# ── Scenario 2: the second compact carries the first ─────────────
scenario_iterative() {
  NEW_WORK iterative
  COMPACT_ENABLED=true
  work_config
  seed_session 4000 4200 4300 6000
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  make_fake_rushi
  FAKE_RUSHI_SUMMARY="the summary of the old region" run_step
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
  echo "$NORMAL_STOP" >"$WORK/plan"
  FAKE_RUSHI_SUMMARY="the summary of the old region, second pass" run_step
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
# fails with the overflow shape, then the overflow compact fires (the
# driver forks the agent), then the retry succeeds. The results carry
# 5600 chars each (1400 tokens) so the walk-back old region is
# non-empty; the usage readings stay under the trigger.
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
  make_fake_rushi
  FAKE_RUSHI_SUMMARY="the overflow summary" run_step
  assert_eq "$(count_events compaction_summary)" 1 "one compaction_summary"
  assert_contains "$(last_event compaction_summary)" '"reason":"overflow"' "the overflow reason rides the marker"
  assert_eq "$(test -f "$SESSIONS_DIR/handoff/v1.md" && echo yes)" "yes" "handoff/v1.md exists"
  assert_eq "$(claim_state)" "idle" "the loop runs to idle"
}

# ── Scenario 4: the branch compact ───────────────────────────────
# The `--branch` run forks the compact agent too. The agent writes
# the versioned branch summary file, not a handoff file. The marker
# is an add-on: it compacts nothing.
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
  make_fake_rushi
  local status
  status=$(
    cd "$WORK"
    export RUSHI_BIN="$WORK/fake-rushi"
    export FAKE_RUSHI_SUMMARY="the branch summary of the ditched span"
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
  # The agent wrote the versioned branch summary file, not a handoff.
  local file
  file="$SESSIONS_DIR/branch-summary/v1.md"
  assert_eq "$(test -f "$file" && echo yes)" "yes" "the versioned branch summary file exists"
  assert_eq "$(cat "$file")" "the branch summary of the ditched span" "the file holds the summary"
  assert_eq "$(cat "$SESSIONS_DIR/handoff.md")" "PRE-EXISTING HANDOFF SENTINEL" "handoff.md is unchanged"
  assert_eq "$(test -d "$SESSIONS_DIR/handoff" && echo yes)" "" "no handoff/ dir for a branch marker"
  # The agent session dir was removed.
  assert_eq "$(find "$WORK/sessions" -maxdepth 1 -name 'session-branch-*' | wc -l)" 0 "the agent session dir was removed"
}

# ── Scenario 5: the compact agent failure (the liveness fallback) ─
# The forked agent session exits non-zero. The driver falls back to the
# bounded model-free cut: the seed transcript becomes the handoff. No
# `compaction_failed` marker; a `compaction_summary` rides the fallback,
# and the handoff file is written. The main session is never stuck.
scenario_compact_failure() {
  NEW_WORK compact-failure
  COMPACT_ENABLED=true
  work_config
  seed_session 6000 6200 6500
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  make_fake_rushi
  FAKE_RUSHI_FAIL=1 run_step
  # The liveness contract: the fallback finishes the compact. No
  # compaction_failed marker; a compaction_summary rides the fallback.
  assert_eq "$(count_events compaction_failed)" 0 "no compaction_failed (the fallback finishes the compact)"
  assert_eq "$(count_events compaction_summary)" 1 "one compaction_summary (the fallback summary)"
  # The fallback handoff is written with the seed transcript.
  assert_eq "$(test -f "$SESSIONS_DIR/handoff/v1.md" && echo yes)" "yes" "the fallback writes the handoff"
  assert_contains "$(cat "$SESSIONS_DIR/handoff/v1.md")" "conversation to summarize" "the fallback handoff is the seed transcript"
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
  make_fake_rushi
  run_step
  assert_eq "$(count_events compaction_summary)" 0 "no compaction_summary"
  assert_eq "$(count_events compaction_failed)" 0 "no compaction_failed"
}

# ── Scenario 7: the explicit `--up-to` boundary ──────────────────
# The driver places the boundary at the explicit seq. The agent
# summarizes the prefix.
scenario_up_to() {
  NEW_WORK up-to
  COMPACT_ENABLED=true
  work_config
  seed_session 6000 6200 6500
  echo "$NORMAL_STOP" >"$WORK/plan"
  make_stub
  make_fake_rushi
  local status
  status=$(
    cd "$WORK"
    export RUSHI_BIN="$WORK/fake-rushi"
    export FAKE_RUSHI_SUMMARY="the up-to summary"
    "$BIN_DIR/compact" "$SESSIONS_DIR" --config "$WORK/config.toml" --force --up-to 4 2>/dev/null
  )
  assert_eq "$(jq -r '.status' <<<"$status")" "compacted" "the forced compact compacts"
  assert_eq "$(jq -r '.first_kept_seq' <<<"$status")" 5 "the boundary lands at seq 5"
  assert_eq "$(test -f "$SESSIONS_DIR/handoff/v1.md" && echo yes)" "yes" "handoff/v1.md exists"
  assert_eq "$(cat "$SESSIONS_DIR/handoff/v1.md")" "the up-to summary" "the file holds the agent summary"
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
