#!/usr/bin/env bash
# The lock-free `rushi run --no-run` e2e (docs/itches.md, 2026-10-01).
#
# Verifies that `rushi run <session> <task> --no-run` is a lock-free
# append, equivalent to `user --session <s> --no-run "<msg>"`:
#
#   - it succeeds while another loop holds the session lock
#   - it appends the steer user_message to the live session log
#   - it writes no `loop.pid` and starts no model turn
#   - a full `rushi run` (no `--no-run`) still fails against a held lock
#   - `--no-run` with no task fails instead of running the loop
#
# A live loop (stub model, one slow turn) holds the lock during the
# poke, matching the monitor pattern in docs/reference/monitoring.md.

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

# ── The work config (same shape as steer-inflight-e2e.sh) ──────────

WORK="$ROOT/scratch/e2e-no-run"
rm -rf "$WORK"
SESSIONS_DIR="$WORK/sessions/session"
SLOG="$SESSIONS_DIR/events.jsonl"
LOCK="$SESSIONS_DIR/.loop.lock"
REQLOG="$WORK/reqlog"
mkdir -p "$SESSIONS_DIR"
: > "$SLOG"

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

# One slow model turn so the loop holds the lock long enough to poke.
cat > "$WORK/stub-model" <<'EOF'
#!/usr/bin/env bash
set -u
if [[ "${1:-}" == "--describe" ]]; then
  echo '{"active":"stub","model_id":"stub-model","reasoning_effort":"none","thinking_level":"off"}'
  exit 0
fi
req=$(cat)
printf '%s\n' "$req" >>"${STUB_REQLOG:-/dev/null}"
sleep 3
echo '{"text":"done","tool_calls":[],"reasoning":[],"stop_reason":"stop","usage":{"input_tokens":10,"output_tokens":10}}'
EOF
chmod +x "$WORK/stub-model"

export CONFIG="$WORK/config.toml"
export MODEL_BIN="$WORK/stub-model"
export STUB_REQLOG="$REQLOG"
: > "$REQLOG"
cd "$WORK"

# Seed the initial task, then start the live loop in the background.
echo '{"v":1,"type":"user_message","ts":"t0","seq":1,"content":"first task"}' > "$SLOG"
rushi_start() { "$BIN_DIR/rushi" run "$@" >"$WORK/run.log" 2>"$WORK/run.err"; }
rushi_start session &
RUN_PID=$!

# Wait for the first model call to be in flight (lock is held now).
for _ in $(seq 1 100); do
  if rg -q '"model_call_context"' "$SLOG" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
if ! rg -q '"model_call_context"' "$SLOG" 2>/dev/null; then
  echo "FAIL: the live loop never reached a model call"
  kill "$RUN_PID" 2>/dev/null
  exit 1
fi

# ── 1. Full run against a held lock still fails ────────────────────
out="$("$BIN_DIR/rushi" run session 2>&1)"
rc=$?
if [ "$rc" -eq 1 ] && echo "$out" | grep -q "session lock is held"; then
  ok "full run fails against a held lock (rc=1, lock message)"
else
  ko "full run against held lock: rc=$rc, output: $out"
fi

# ── 2. --no-run succeeds against a held lock ────────────────────────
out="$("$BIN_DIR/rushi" run session "poke message" --no-run 2>&1)"
rc=$?
if [ "$rc" -eq 0 ] && echo "$out" | grep -q '"content":"poke message"'; then
  ok "--no-run succeeds against a held lock (rc=0, event line printed)"
else
  ko "--no-run against held lock: rc=$rc, output: $out"
fi

if rg -q '"content":"poke message"' "$SLOG"; then
  ok "the poke was appended to the live session log"
else
  ko "the poke is missing from the live session log"
fi

# ── 3. --no-run without a task fails instead of running the loop ───
"$BIN_DIR/rushi" run session --no-run < /dev/null >/dev/null 2>&1
rc=$?
if [ "$rc" -eq 1 ]; then
  ok "--no-run without a task fails (rc=1)"
else
  ko "--no-run without a task: rc=$rc (want 1)"
fi

out="$("$BIN_DIR/rushi" run session "" --no-run 2>&1)"
rc=$?
if [ "$rc" -eq 1 ] && echo "$out" | grep -q "task must not be empty"; then
  ok "an empty task fails with the empty-task message"
else
  ko "empty task --no-run: rc=$rc, output: $out"
fi

# Wait for the live loop to finish (it drains the poke at step 2).
for _ in $(seq 1 100); do
  if ! kill -0 "$RUN_PID" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
if kill -0 "$RUN_PID" 2>/dev/null; then
  kill "$RUN_PID" 2>/dev/null
  ko "the live loop did not finish in time"
fi
wait "$RUN_PID" 2>/dev/null

# Two model calls: the second one must carry the poked message.
REQS=$(wc -l < "$REQLOG")
SECOND=$(sed -n '2p' "$REQLOG")
if [ "$REQS" -eq 2 ] && echo "$SECOND" | grep -q "poke message"; then
  ok "the live loop drained the poked message (2 model calls)"
else
  ko "drain check: ${REQS} model calls, second: ${SECOND:-none}"
fi

# ── 4. --no-run on a dead session: no loop.pid, no model call ──────
# Clear the pid + reqlog left by the live loop so the checks are clean.
rm -f "$SESSIONS_DIR/loop.pid" "$REQLOG"
"$BIN_DIR/rushi" run session "seed only" --no-run >/dev/null 2>&1
rc=$?
if [ "$rc" -eq 0 ] && [ ! -e "$SESSIONS_DIR/loop.pid" ]; then
  ok "--no-run writes no loop.pid on a dead session"
else
  lp_exists=no
  [ -e "$SESSIONS_DIR/loop.pid" ] && lp_exists=yes
  ko "--no-run on dead session: rc=$rc, loop.pid exists: $lp_exists"
fi

echo
echo "no-run-lock-free-e2e: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
