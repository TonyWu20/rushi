#!/usr/bin/env bash
# The `rushi run` final-message e2e (docs/reference/headless-missions.md,
# "Result on stdout").
#
# On a clean idle stop, `rushi run` prints the content of the last
# `assistant_message` to stdout at the moment the loop stops. This
# script verifies the print contract against a stub model:
#
#   - a dead-session run exits 0 and prints the stub model's final
#     text on stdout (the command-substitution and the redirect
#     forms)
#   - a bare run on an idle session without an assistant message
#     prints nothing
#   - a failed model stop prints nothing: the stop is not an idle
#     stop, and the session log holds the record

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

# ── The work config (same shape as run-lock-branch-e2e.sh) ────────

WORK="$ROOT/scratch/e2e-run-final-message"
rm -rf "$WORK"
SESSIONS_DIR="$WORK/sessions"
mkdir -p "$SESSIONS_DIR"

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

# The stub model: one turn, a two-line final text. The second line
# proves the multi-line content survives the print round trip.
cat > "$WORK/stub-model" <<'STUB'
#!/usr/bin/env bash
set -u
if [[ "${1:-}" == "--describe" ]]; then
  echo '{"active":"stub","model_id":"stub-model","reasoning_effort":"none","thinking_level":"off"}'
  exit 0
fi
req=$(cat)
printf '%s\n' "$req" >>"${STUB_REQLOG:-/dev/null}"
echo '{"text":"mission result: 42\ndone at step 3","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":10}}'
STUB
chmod +x "$WORK/stub-model"

export CONFIG="$WORK/config.toml"
export MODEL_BIN="$WORK/stub-model"
export STUB_REQLOG="$WORK/reqlog"
: > "$STUB_REQLOG"
cd "$WORK"

RUSHI="$BIN_DIR/rushi"

# ── 1. Dead session: the capture form carries the final message ───
out="$("$RUSHI" run mission "mission task" 2>"$WORK/run1.err")"
rc=$?
EXPECTED=$'mission result: 42\ndone at step 3'
if [ "$rc" -eq 0 ] && [ "$out" = "$EXPECTED" ]; then
  ok "a clean idle stop prints the last assistant message (capture form)"
else
  ko "idle stop capture: rc=$rc, output: $(printf '%q' "$out")"
fi

# ── 2. The same session, the redirect form ────────────────────────
"$RUSHI" run mission "second mission" >"$WORK/cap" 2>"$WORK/run2.err"
rc=$?
CAP_CONTENT=$(cat "$WORK/cap")
if [ "$rc" -eq 0 ] && [ "$CAP_CONTENT" = "$EXPECTED" ]; then
  ok "the redirect form carries the final message to the file"
else
  ko "idle stop redirect: rc=$rc, file: $(printf '%q' "$CAP_CONTENT")"
fi

# ── 3. A bare run on an idle session with no assistant message ────
out="$("$RUSHI" run fresh 2>"$WORK/run3.err")"
rc=$?
if [ "$rc" -eq 0 ] && [ -z "$out" ]; then
  ok "an idle stop without an assistant message prints nothing"
else
  ko "idle stop without a message: rc=$rc, output: $(printf '%q' "$out")"
fi

# ── 4. A failed model stop prints nothing ──────────────────────────
cat > "$WORK/stub-model" <<'STUB'
#!/usr/bin/env bash
req=$(cat)
printf '%s\n' "$req" >>"${STUB_REQLOG:-/dev/null}"
echo "stub model failure" >&2
exit 3
STUB
chmod +x "$WORK/stub-model"

out="$("$RUSHI" run failing "doomed task" 2>"$WORK/run4.err")"
rc=$?
if [ "$rc" -eq 0 ] && [ -z "$out" ]; then
  ok "a failed model stop prints nothing (the log holds the record)"
else
  ko "failed stop: rc=$rc, output: $(printf '%q' "$out")"
fi

echo
echo "run-final-message-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
