#!/usr/bin/env bash
# The model.before transform e2e (docs/loop-lifecycle-hooks.md 12.6,
# issue #38, pipeline model).
# Drives `rushi step` with a scriptable stub model and jq-based
# model.before pipeline steps. Each step receives the accumulated
# request JSON on stdin and writes the next request to stdout (exit
# 0). Asserts the composed transform, the `hook_applied` markers,
# the chain markers, the fail/abort stop paths, and the
# byte-identical no-hooks default.

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
# window 8000, output 4096, input budget 3904. A single small turn
# stays far under the trigger level, so no compact fires.

# $1 = a non-empty value adds the [hooks] pipeline registration to
# the config; an empty value leaves the config hook-free.
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
compact_enabled = false

[system_prompt]
text = "test"
EOF
  if [[ -n "${1:-}" ]]; then
    cat >>"$WORK/config.toml" <<EOF
[hooks]
timeout_ms = 30000

$1
EOF
  fi
}

# The single-step pipeline registration. Expanded at call time,
# after `NEW_WORK` sets `WORK`.
single_pipeline() {
  cat <<EOF
[hooks.defs.hook-model-before]
command = "$WORK/hook-model-before"

[hooks.pipeline."model.before"]
steps = ["hook-model-before"]
EOF
}

# The two-step pipeline (P7): hook-a, then hook-b.
two_pipeline() {
  cat <<EOF
[hooks.defs.hook-a]
command = "$WORK/hook-a"

[hooks.defs.hook-b]
command = "$WORK/hook-b"

[hooks.pipeline."model.before"]
steps = ["hook-a", "hook-b"]
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

# ── The stub model ─────────────────────────────────────────────────
# Records every normal request on STUB_REQLOG, one JSON line each,
# and answers a final stop.
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

# ── The model.before pipeline steps ────────────────────────────────
# The pipeline ABI (12.3/12.6): stdin is the accumulated request
# object, stdout is the step's replacement request, exit 0 = ok.

# The transform step: prepend a goal-continuation instruction to the
# request input, as a pi-goal-style continuation prompt would.
make_hook() {
  cat > "$WORK/hook-model-before" <<'EOF'
#!/usr/bin/env bash
set -u
req=$(cat)
jq -c '.input = ([{"type":"message","role":"user","content":"goal-hook: continue the active goal"}] + .input)' <<<"$req"
EOF
  chmod +x "$WORK/hook-model-before"
}

# P7 compose step A: prepend its own marker message.
make_hook_a() {
  cat > "$WORK/hook-a" <<'EOF'
#!/usr/bin/env bash
set -u
req=$(cat)
jq -c '.input = ([{"type":"message","role":"user","content":"A: first step prepends"}] + .input)' <<<"$req"
EOF
  chmod +x "$WORK/hook-a"
}

# P7 compose step B: step B receives step A's output on its stdin;
# it echoes what it saw back into the request, proving the
# composition (step N+1 receives step N's output).
make_hook_b() {
  cat > "$WORK/hook-b" <<'EOF'
#!/usr/bin/env bash
set -u
req=$(cat)
seen=$(jq -r '.input[0].content // ""' <<<"$req")
jq -c --arg seen "$seen" '.input = ([{"type":"message","role":"user","content":("B saw: " + $seen)}] + .input)' <<<"$req"
EOF
  chmod +x "$WORK/hook-b"
}

# The legacy-shape step: a pre-cutover hook still emitting the
# `{"decision":"transform"}` envelope. Its stdout is honored as the
# step's state, but it is not a request object (no `input` field),
# so the kernel logs `hook.model.before.error` and the original
# request proceeds (P4, 12.8 near-compatibility).
make_hook_legacy() {
  cat > "$WORK/hook-model-before" <<'EOF'
#!/usr/bin/env bash
set -u
cat > /dev/null
echo '{"decision":"transform"}'
EOF
  chmod +x "$WORK/hook-model-before"
}

# The no-op step (issue #24): the request echoed back unchanged.
make_hook_noop() {
  cat > "$WORK/hook-model-before" <<'EOF'
#!/usr/bin/env bash
set -u
cat
EOF
  chmod +x "$WORK/hook-model-before"
}

# The failing step (P4): exit 3 with a detail.
make_hook_fail() {
  cat > "$WORK/hook-model-before" <<'EOF'
#!/usr/bin/env bash
set -u
cat > /dev/null
echo '{"reason":"stub exploded"}'
exit 3
EOF
  chmod +x "$WORK/hook-model-before"
}

run_step() {
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_REQLOG="$WORK/reqlog"
    : >"$STUB_REQLOG"
    "$BIN_DIR/rushi" step session
  ) >/dev/null 2>&1
  true
}

claim_state() {
  "$BIN_DIR/claim" --session "$SESSIONS_DIR" | jq -r .state
}

last_req() {
  tail -1 "$STUB_REQLOG" 2>/dev/null || true
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
  WORK="$ROOT/scratch/e2e-model-before/$1"
  rm -rf "$WORK"
  SESSIONS_DIR="$WORK/sessions/session"
  SLOG="$SESSIONS_DIR/events.jsonl"
  STUB_REQLOG="$WORK/reqlog"
  mkdir -p "$SESSIONS_DIR"
  echo "==== scenario: $1"
}

# ── Scenario 1: the transform step ────────────────────────────────
# The step transforms the request: the model receives the injected
# instruction, and the log carries the resolution and cache-break
# markers plus the chain marker.
scenario_transform() {
  NEW_WORK transform
  work_config "$(single_pipeline)"
  seed_session
  make_stub
  make_hook
  run_step
  assert_eq "$(claim_state)" "idle" "the step runs to idle"
  local req
  req="$(last_req)"
  assert_eq "$(jq -r '.input[0].content' <<<"$req")" \
    "goal-hook: continue the active goal" \
    "the model receives the transformed request"
  assert_eq "$(count_markers "hook.model.before")" 1 "one hook.model.before marker"
  assert_eq "$(marker_values "hook.model.before" | head -1)" '"transform"' \
    "the resolution marker carries the outcome"
  assert_eq "$(count_markers "hook.model.before.chain")" 1 "one chain marker"
  assert_eq "$(jq -r 'select(.id == "hook.model.before.chain") | .value.stop_kind' "$SLOG" 2>/dev/null)" \
    "complete" "the chain completed"
  assert_eq "$(jq -c 'select(.id == "hook.model.before.chain") | .value.steps' "$SLOG" 2>/dev/null)" \
    '["ok"]' "the chain marker records the step outcome"
  assert_eq "$(count_markers "hook_applied")" 1 "one hook_applied marker"
  assert_eq "$(marker_values "hook_applied" | head -1)" \
    "\"$WORK/hook-model-before\"" "the marker names the hook command"
}

# ── Scenario 2: two transforms compose (P7) ───────────────────────
# Step B receives step A's output; the model sees B's marker, which
# quotes what B read from A's output. One hook_applied marker per
# ok step, in step order.
scenario_compose() {
  NEW_WORK compose
  work_config "$(two_pipeline)"
  seed_session
  make_stub
  make_hook_a
  make_hook_b
  run_step
  assert_eq "$(claim_state)" "idle" "the step runs to idle"
  local req
  req="$(last_req)"
  assert_eq "$(jq -r '.input[0].content' <<<"$req")" \
    "B saw: A: first step prepends" \
    "step B saw step A's output (matrix-product composition)"
  assert_eq "$(count_markers "hook.model.before.chain")" 1 "one chain marker"
  assert_eq "$(jq -r 'select(.id == "hook.model.before.chain") | .value.stop_kind' "$SLOG" 2>/dev/null)" \
    "complete" "the chain completed"
  assert_eq "$(jq -c 'select(.id == "hook.model.before.chain") | .value.steps' "$SLOG" 2>/dev/null)" \
    '["ok","ok"]' "the chain marker records every step's outcome"
  assert_eq "$(count_markers "hook_applied")" 2 "one marker per ok step"
  assert_eq "$(marker_values "hook_applied" | sed -n 1p)" "\"$WORK/hook-a\"" \
    "the first marker names step A"
  assert_eq "$(marker_values "hook_applied" | sed -n 2p)" "\"$WORK/hook-b\"" \
    "the second marker names step B (step order)"
}

# ── Scenario 3: the no-hooks default ──────────────────────────────
# No pipeline entry: zero steps run, the request goes out
# untouched, and no hook markers appear in the log (P1, the
# byte-identical no-hooks default).
scenario_default() {
  NEW_WORK default
  work_config ""
  seed_session
  make_stub
  run_step
  assert_eq "$(claim_state)" "idle" "the step runs to idle"
  local req
  req="$(last_req)"
  assert_eq "$(jq -r '.input[0].content' <<<"$req")" "do the task" \
    "the model receives the unmodified request"
  assert_eq "$(count_markers "hook.model.before")" 0 "no resolution marker"
  assert_eq "$(count_markers "hook.model.before.chain")" 0 "no chain marker"
  assert_eq "$(count_markers "hook_applied")" 0 "no cache-break marker"
}

# ── Scenario 4: the legacy envelope degrades to an error ──────────
# A pre-cutover step still emitting the `{"decision":...}` envelope
# is honored on the data channel: its stdout is the step's state,
# which is not a request object, so the window fails non-blocking —
# the original request proceeds and the log carries the error.
scenario_legacy_envelope() {
  NEW_WORK legacy
  work_config "$(single_pipeline)"
  seed_session
  make_stub
  make_hook_legacy
  run_step
  assert_eq "$(claim_state)" "idle" "the step runs to idle"
  local req
  req="$(last_req)"
  assert_eq "$(jq -r '.input[0].content' <<<"$req")" "do the task" \
    "the model receives the unmodified request"
  assert_eq "$(count_markers "hook.model.before.error")" 1 "one error marker"
  assert_eq "$(count_markers "hook.model.before.chain")" 1 "the chain marker is still logged"
  assert_eq "$(count_markers "hook_applied")" 0 "no cache-break marker"
}

# ── Scenario 5: no-op step (issue #24) ────────────────────────────
# The step echoes the request unchanged: no `hook_applied` marker,
# but the resolution and chain markers are still logged.
scenario_noop() {
  NEW_WORK noop
  work_config "$(single_pipeline)"
  seed_session
  make_stub
  make_hook_noop
  run_step
  assert_eq "$(claim_state)" "idle" "the step runs to idle"
  local req
  req="$(last_req)"
  assert_eq "$(jq -r '.input[0].content' <<<"$req")" "do the task" \
    "the model receives the unmodified request"
  assert_eq "$(count_markers "hook.model.before")" 1 \
    "one resolution marker (the noop outcome is logged)"
  assert_eq "$(marker_values "hook.model.before" | head -1)" '"noop"' \
    "the resolution marker carries the noop outcome"
  assert_eq "$(count_markers "hook_applied")" 0 \
    "no cache-break marker on a no-op step"
}

# ── Scenario 6: a failing step (P4) ──────────────────────────────
# exit 3 stops the chain; the window falls back to its default:
# the original request proceeds, the log carries the error and
# chain markers.
scenario_fail() {
  NEW_WORK fail
  work_config "$(single_pipeline)"
  seed_session
  make_stub
  make_hook_fail
  run_step
  assert_eq "$(claim_state)" "idle" "the step runs to idle"
  local req
  req="$(last_req)"
  assert_eq "$(jq -r '.input[0].content' <<<"$req")" "do the task" \
    "the model receives the unmodified request"
  assert_eq "$(count_markers "hook.model.before.error")" 1 "one error marker"
  assert_eq "$(jq -c 'select(.id == "hook.model.before.chain") | .value.stop_kind' "$SLOG" 2>/dev/null)" \
    '"fail"' "the chain marker records the fail stop"
  assert_eq "$(count_markers "hook_applied")" 0 "no cache-break marker"
}

scenario_transform
scenario_compose
scenario_default
scenario_legacy_envelope
scenario_noop
scenario_fail

echo
echo "model-before-transform-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
