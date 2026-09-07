#!/usr/bin/env bash
# lean-verify-drt-e2e.sh — offline behavior test of lean-verify's DRT
# machinery. No nix / lake / Lean toolchain needed: the "model" and
# "prod" sides are plain shell commands, so the regression-gate logic
# (smoke tier, progress/checkpoint file, resume, check-inputs) is
# exercised directly. The Lean-side end-to-end flow (init/build with
# a real lake project) lives in scripts/lean-verify-e2e.sh, which
# SKIPs when the nix devShells are absent.
#
#   1. drt pass          — a shell echo pair must match on every input
#   2. drt smoke tier    — "smoke":true is the n=2000 quick preset
#   3. progress lifecycle — heartbeat file written while running,
#                          deleted on a clean run
#   4. kill -9 + resume  — a killed run is picked up from its checkpoint
#   5. mismatch + fix    — a divergent prod stops the run and keeps a
#                          checkpoint; fixing prod + resume re-checks
#                          from the first failed index and passes
#   6. resume mismatch   — resuming with changed parameters is refused
#   7. check-inputs ok   — all lines accepted by both sides
#   8. check-inputs bad  — the first rejected line is reported (with
#                          progress, a stopped run resumes from it)
#   9. live-run guard    — a fresh run refuses to clobber a live
#                          checkpoint
#
# Requires: a built tool (`cargo build -p lean-verify`) and jq.
# Exit 0 = all assertions passed, 1 = a gate failed.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug/lean-verify"
[ -x "$BIN" ] || BIN="$ROOT/target/release/lean-verify"
if [ ! -x "$BIN" ]; then
  echo "FAIL: lean-verify binary not built (cargo build -p lean-verify)"
  exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "SKIP: jq not available; cannot assert on tool results"
  exit 0
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/lean-verify-drt-e2e.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

# JSON-escape a shell command (jq does the quoting safely).
jesc() { printf '%s' "$1" | jq -Rs .; }

FAILED=0
check() {
  local name="$1" got="$2" want="$3"
  if [ "$got" != "$want" ]; then
    echo "FAIL: $name — expected '$want', got '$got'"
    FAILED=1
  else
    echo "PASS: $name"
  fi
}

# The fixtures. The sides sleep a little so long-enough runs can be
# observed (and killed) mid-flight.
ECHO_CMD='sleep 0.02; printf %s "$1"'
MODEL=$(jesc "$ECHO_CMD")
cat > prod.sh <<'EOS'
#!/bin/sh
sleep 0.02
printf 'X%s' "$1"
EOS
chmod +x "$WORK/prod.sh"
PROD=$(jesc "\"$WORK/prod.sh\" \"\$1\"")
cat > reject.sh <<'EOS'
#!/bin/sh
case "$1" in
  7) printf 'parse error: not a scenario' 1>&2; exit 3 ;;
  *) printf '%s' "$1" ;;
esac
EOS
chmod +x "$WORK/reject.sh"
REJECT=$(jesc "\"$WORK/reject.sh\" \"\$1\"")

call() { printf '%s' "$1" | "$BIN"; }
# Run a call capturing stderr (the tool's tool-level diagnostics).
call_err() {
  local json="$1"
  CALL_ERR=$(printf '%s' "$json" | "$BIN" 2>&1 >/dev/null)
  CALL_RC=$?
}

# 1. drt pass — echo vs echo, 200 inputs.
cat > prodfast.sh <<'EOS'
#!/bin/sh
printf %s "$1"
EOS
chmod +x "$WORK/prodfast.sh"
PRODF=$(jesc "\"$WORK/prodfast.sh\" \"\$1\"")
FAST_MODEL='printf %s "$1"'
J1=$(printf '{"op":"drt","model":%s,"prod":%s,"input_gen":"seq 1 200","n":200}' \
  "$(jesc "$FAST_MODEL")" "$PRODF")
OUT1=$(call "$J1")
check "drt pass: all inputs match" "$(printf '%s' "$OUT1" | jq -r '.pass')" "true"
check "drt pass: checked 200" "$(printf '%s' "$OUT1" | jq -r '.checked')" "200"

# 2. drt smoke tier — "smoke":true, no explicit n => n=2000, tier smoke.
J2=$(printf '{"op":"drt","model":%s,"prod":%s,"smoke":true}' "$(jesc "$FAST_MODEL")" "$PRODF")
OUT2=$(call "$J2")
check "drt smoke: tier smoke" "$(printf '%s' "$OUT2" | jq -r '.tier')" "smoke"
check "drt smoke: passed" "$(printf '%s' "$OUT2" | jq -r '.pass')" "true"
check "drt smoke: checked 2000" "$(printf '%s' "$OUT2" | jq -r '.checked')" "2000"

# 3. progress lifecycle — clean run deletes the heartbeat file.
J3="$J1"
call "$J3" >/dev/null
if [ -e "$WORK/.drt-progress.json" ]; then
  check "progress: clean run removes the file" "present" "absent"
else
  check "progress: clean run removes the file" "absent" "absent"
fi

# 4. kill -9 + resume — a killed run is resumed from its checkpoint.
J4=$(printf '{"op":"drt","model":%s,"prod":%s,"input_gen":"seq 1 200","n":200}' \
  "$MODEL" "$PRODF")
# Background the tool itself (stdin from a file) so $! is the tool's
# pid: a backgrounded `printf | tool` pipeline would put the subshell
# in $! and the kill would miss the tool.
printf '%s' "$J4" > j4.json
"$BIN" < j4.json > run4.json &
BG_PID=$!
# Wait until the run is actually processing inputs (heartbeat written).
for _ in $(seq 1 120); do
  if [ -f .drt-progress.json ] && [ "$(jq -r '.processed' .drt-progress.json 2>/dev/null || echo 0)" -ge 50 ]; then
    break
  fi
  sleep 0.25
done
check "progress: heartbeat alive" "$(jq -r '.status' .drt-progress.json)" "running"
KILL_NEXT=$(jq -r '.next' .drt-progress.json)
kill -9 "$BG_PID" 2>/dev/null || true
wait "$BG_PID" 2>/dev/null || true
check "kill: checkpoint kept, status running" "$(jq -r '.status' .drt-progress.json)" "running"
RES4=$(printf '%s' "$J4" | jq '. + {"resume":true}')
OUT4=$(call "$RES4")
check "resume: pass after kill" "$(printf '%s' "$OUT4" | jq -r '.pass')" "true"
check "resume: resumed from checkpoint" "$(printf '%s' "$OUT4" | jq -r '.resumed_from')" "$KILL_NEXT"
check "resume: all 200 checked" "$(printf '%s' "$OUT4" | jq -r '.checked')" "200"
if [ -e "$WORK/.drt-progress.json" ]; then
  check "resume: clean completion removes checkpoint" "present" "absent"
else
  check "resume: clean completion removes checkpoint" "absent" "absent"
fi

# 5. mismatch + fix + resume — the prod side diverges (prefix "X"):
#    the run stops at the first mismatch and keeps a checkpoint.
J5=$(printf '{"op":"drt","model":%s,"prod":%s,"input_gen":"seq 1 32","n":32}' \
  "$(jesc "$FAST_MODEL")" "$PROD")
OUT5=$(call "$J5")
check "mismatch: gate red" "$(printf '%s' "$OUT5" | jq -r '.pass')" "false"
check "mismatch: stop reason" "$(printf '%s' "$OUT5" | jq -r '.stop_reason')" "mismatch"
check "mismatch: checkpoint kept" "$(printf '%s' "$OUT5" | jq -r '.checkpoint')" "true"

# 6. resume with changed parameters is refused (tool-level failure).
BAD6=$(printf '%s' "$J5" | jq '. + {"n":31,"resume":true}')
call_err "$BAD6"
check "resume mismatch: refused" "$CALL_RC" "1"
case "$CALL_ERR" in
  *"checkpoint parameter mismatch"*"n"*) check "resume mismatch: names the changed field" "n" "n" ;;
  *) check "resume mismatch: names the changed field" "$CALL_ERR" "n" ;;
esac

# 5b. The "fix": the production binary is rebuilt — now it echoes.
cat > prod.sh <<'EOS'
#!/bin/sh
sleep 0.02
printf '%s' "$1"
EOS
chmod +x "$WORK/prod.sh"
OUT5B=$(call "$(printf '%s' "$J5" | jq '. + {"resume":true}')")
check "fix+resume: pass" "$(printf '%s' "$OUT5B" | jq -r '.pass')" "true"
check "fix+resume: resumed from 0" "$(printf '%s' "$OUT5B" | jq -r '.resumed_from')" "0"
check "fix+resume: all 32 checked" "$(printf '%s' "$OUT5B" | jq -r '.checked')" "32"

# 7. check-inputs ok — every generated line is accepted by both sides.
J7=$(printf '{"op":"check-inputs","model":%s,"prod":%s,"input_gen":"seq 1 8","n":8}' \
  "$(jesc "$FAST_MODEL")" "$(jesc "$FAST_MODEL")")
OUT7=$(call "$J7")
check "check-inputs: ok" "$(printf '%s' "$OUT7" | jq -r '.ok')" "true"
check "check-inputs: checked 8" "$(printf '%s' "$OUT7" | jq -r '.checked')" "8"

# 8. check-inputs bad — the generator emits a line the model side
#    rejects (exit 3). With progress on, the stop keeps a checkpoint
#    that resumes from the first rejected line.
J8=$(printf '{"op":"check-inputs","model":%s,"prod":%s,"input_gen":"seq 1 8","n":8,"progress":true}' \
  "$REJECT" "$(jesc "$FAST_MODEL")")
OUT8=$(call "$J8")
check "check-inputs: not ok" "$(printf '%s' "$OUT8" | jq -r '.ok')" "false"
check "check-inputs: stop reason reject" "$(printf '%s' "$OUT8" | jq -r '.stop_reason')" "reject"
check "check-inputs: first rejected input" "$(printf '%s' "$OUT8" | jq -r '.rejected[0].input')" "7"
check "check-inputs: model exit 3" "$(printf '%s' "$OUT8" | jq -r '.rejected[0].model.exit')" "3"
check "check-inputs: checked 7" "$(printf '%s' "$OUT8" | jq -r '.checked')" "7"
OUT8R=$(call "$(printf '%s' "$J8" | jq '. + {"resume":true}')")
check "check-inputs resume: from 6" "$(printf '%s' "$OUT8R" | jq -r '.resumed_from')" "6"
check "check-inputs resume: still not ok" "$(printf '%s' "$OUT8R" | jq -r '.ok')" "false"

# 9. live-run guard — a fresh run must not clobber a live checkpoint.
J9=$(printf '{"op":"drt","model":%s,"prod":%s,"input_gen":"seq 1 200","n":200}' \
  "$MODEL" "$PRODF")
printf '%s' "$J9" > j9.json
"$BIN" < j9.json > run9.json &
BG_PID=$!
for _ in $(seq 1 120); do
  [ -f .drt-progress.json ] && break
  sleep 0.25
done
J9F=$(printf '{"op":"drt","model":%s,"prod":%s,"input_gen":"seq 1 50","n":50}' \
  "$(jesc "$FAST_MODEL")" "$PRODF")
call_err "$J9F"
check "live guard: fresh run refused" "$CALL_RC" "1"
case "$CALL_ERR" in
  *"another lean-verify run"*) check "live guard: named the reason" "named" "named" ;;
  *) check "live guard: named the reason" "$CALL_ERR" "named" ;;
esac
wait "$BG_PID" 2>/dev/null || true
OUT9=$(jq -r '.pass' run9.json 2>/dev/null || echo "")
check "live guard: background run passed" "$OUT9" "true"

if [ $FAILED -ne 0 ]; then
  echo "FAIL: lean-verify drt machinery e2e (assertions above)"
  exit 1
fi
echo "PASS: lean-verify drt machinery e2e (pass, smoke, progress, kill+resume, fix+resume, check-inputs, live guard)"
