#!/usr/bin/env bash
# The tool.after transform e2e (docs/image-read-kiss.md section 4.5,
# pipeline ABI per docs/loop-lifecycle-hooks.md 12.5, issue #38).
# Drives `rushi step` with a scriptable stub model and a jq-based
# tool.after pipeline step. The step receives the accumulated state
# (the routed calls and results) on stdin and may emit a `results`
# map rewriting results by call id. Asserts the splice, the
# resolution and chain markers, the fail path, and the
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

# $1 = non-empty adds the tool.after pipeline to the config.
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

[hooks.defs.hook-tool-after]
command = "$WORK/hook-tool-after"

[hooks.pipeline."tool.after"]
steps = ["hook-tool-after"]
EOF
  fi
}

# ── The session builder ───────────────────────────────────────────
seed_session() {
  cat > "$SLOG" <<EOF
{"v":1,"type":"user_message","ts":"t1","seq":1,"content":"read the file"}
EOF
}

# ── The stub model ─────────────────────────────────────────────────
# First call: emit a tool_call for `read`. Second call: final answer.
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
n=$(rg -c '"type":"tool_result"' <<<"$req" 2>/dev/null || echo 0)
if [[ "$n" -eq 0 ]]; then
  echo '{"text":"","tool_calls":[{"id":"call-1","name":"read","arguments":"{\"file_path\":\"/nonexistent\"}"}],"reasoning":[],"stop_reason":"tool_calls","usage":{"input_tokens":10,"output_tokens":10}}'
else
  echo '{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":20,"output_tokens":10}}'
fi
EOF
  chmod +x "$WORK/stub-model"
}

# ── The tool.after pipeline step ──────────────────────────────────
# The pipeline ABI (12.3): stdin is the accumulated state (the
# routed calls + results), stdout is the step's state. A `results`
# map rewrites results by call id; unmentioned calls pass through.
make_hook() {
  cat > "$WORK/hook-tool-after" <<'EOF'
#!/usr/bin/env bash
set -u
payload=$(cat)
jq -cn '{
  results: {
    "call-1": {
      "v": 1,
      "type": "tool_result",
      "id": "call-1",
      "ts": "2026-01-01T00:00:00Z",
      "value": {
        "text": "Read image file [image/png] (4242 bytes)",
        "details": {
          "type": "image",
          "mime_type": "image/png",
          "data": "AAAA",
          "path": "/nonexistent"
        }
      },
      "is_error": false
    }
  },
  reason: "e2e: replaced read result with image"
}'
EOF
  chmod +x "$WORK/hook-tool-after"
}

# The no-op step: empty state contribution, the routed results stand.
make_hook_noop() {
  cat > "$WORK/hook-tool-after" <<'EOF'
#!/usr/bin/env bash
set -u
cat > /dev/null
echo '{}'
EOF
  chmod +x "$WORK/hook-tool-after"
}

# The failing step (P4): exit 3 — the routed results stand
# unchanged and the error marker is logged.
make_hook_fail() {
  cat > "$WORK/hook-tool-after" <<'EOF'
#!/usr/bin/env bash
set -u
cat > /dev/null
echo '{"reason":"rewriter exploded"}'
exit 3
EOF
  chmod +x "$WORK/hook-tool-after"
}

run_step() {
  (
    cd "$WORK"
    export CONFIG="$WORK/config.toml"
    export MODEL_BIN="$WORK/stub-model"
    export STUB_REQLOG="$WORK/reqlog"
    : >"$STUB_REQLOG"
    # One step: model call, tool routing, then the tool.after
    # pipeline. The step lands at awaiting_model. The next model
    # call is owed.
    "$BIN_DIR/rushi" step session
  ) >/dev/null 2>&1
  true
}

claim_state() {
  "$BIN_DIR/claim" --session "$SESSIONS_DIR" | jq -r .state
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
  WORK="$ROOT/scratch/e2e-tool-after/$1"
  rm -rf "$WORK"
  SESSIONS_DIR="$WORK/sessions/session"
  SLOG="$SESSIONS_DIR/events.jsonl"
  STUB_REQLOG="$WORK/reqlog"
  mkdir -p "$SESSIONS_DIR"
  echo "==== scenario: $1"
}

# ── Scenario 1: the rewrite ────────────────────────────────────────
# The step replaces the call-1 result. The log carries the replaced
# result, not the original error.
scenario_transform() {
  NEW_WORK transform
  work_config 1
  seed_session
  make_stub
  make_hook
  run_step
  assert_eq "$(claim_state)" "awaiting_model" "step lands at awaiting_model (tool result logged)"
  # The log should contain the replaced result, not the original error.
  local replaced
  replaced=$(jq -c 'select(.type == "tool_result" and .id == "call-1")' "$SLOG" 2>/dev/null)
  assert_eq "$(jq -r '.value.details.type' <<<"$replaced")" \
    "image" \
    "the tool_result in the log is the replaced image result"
  assert_eq "$(jq -r '.value.details.mime_type' <<<"$replaced")" \
    "image/png" \
    "the replaced result carries the mime_type"
  assert_eq "$(jq -r '.is_error' <<<"$replaced")" \
    "false" \
    "the replaced result is not an error"
  # The resolution marker is logged.
  assert_eq "$(count_markers "hook.tool.after")" 1 "one hook.tool.after marker"
  assert_eq "$(jq -r "select(.type == \"ext_status\" and .id == \"hook.tool.after\") | .value" "$SLOG")" \
    "transform" "the resolution marker carries the outcome"
  assert_eq "$(jq -c 'select(.id == "hook.tool.after.chain") | .value.steps' "$SLOG" 2>/dev/null)" \
    '["ok"]' "the chain marker records the step outcome"
}

# ── Scenario 2: the no-hooks default ──────────────────────────────
# No pipeline entry: zero steps, the tool result goes through
# untouched, no markers (P1).
scenario_default() {
  NEW_WORK default
  work_config ""
  seed_session
  make_stub
  run_step
  assert_eq "$(claim_state)" "awaiting_model" "step lands at awaiting_model (tool result logged)"
  local orig
  orig=$(jq -c 'select(.type == "tool_result" and .id == "call-1")' "$SLOG" 2>/dev/null)
  assert_eq "$(jq -r '.is_error' <<<"$orig")" \
    "true" \
    "the original error result is in the log"
  assert_eq "$(count_markers "hook.tool.after")" 0 "no resolution marker"
  assert_eq "$(count_markers "hook.tool.after.chain")" 0 "no chain marker"
}

# ── Scenario 3: the no-op step ─────────────────────────────────────
# The step contributes no state: nothing is spliced, the original
# results stand, the resolution is `noop`.
scenario_noop() {
  NEW_WORK noop
  work_config 1
  seed_session
  make_stub
  make_hook_noop
  run_step
  assert_eq "$(claim_state)" "awaiting_model" "step lands at awaiting_model (tool result logged)"
  local orig
  orig=$(jq -c 'select(.type == "tool_result" and .id == "call-1")' "$SLOG" 2>/dev/null)
  assert_eq "$(jq -r '.is_error' <<<"$orig")" \
    "true" \
    "the no-op step leaves the original result"
  assert_eq "$(count_markers "hook.tool.after")" 1 "one hook.tool.after marker"
  assert_eq "$(jq -r "select(.type == \"ext_status\" and .id == \"hook.tool.after\") | .value" "$SLOG")" \
    "noop" "the resolution marker carries the noop outcome"
}

# ── Scenario 4: the failing step (P4) ─────────────────────────────
# A step failure stops the chain; the routed results stand and the
# error marker names the step and detail.
scenario_fail() {
  NEW_WORK fail
  work_config 1
  seed_session
  make_stub
  make_hook_fail
  run_step
  assert_eq "$(claim_state)" "awaiting_model" "step lands at awaiting_model (tool result logged)"
  local orig
  orig=$(jq -c 'select(.type == "tool_result" and .id == "call-1")' "$SLOG" 2>/dev/null)
  assert_eq "$(jq -r '.is_error' <<<"$orig")" \
    "true" \
    "a failed step leaves the original result"
  assert_eq "$(count_markers "hook.tool.after.error")" 1 "one error marker"
  assert_eq "$(jq -c 'select(.id == "hook.tool.after.chain") | .value.steps' "$SLOG" 2>/dev/null)" \
    '["fail(rewriter exploded)"]' \
    "the chain marker records the fail with its detail"
}

scenario_transform
scenario_default
scenario_noop
scenario_fail

echo
echo "tool-after-transform-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
