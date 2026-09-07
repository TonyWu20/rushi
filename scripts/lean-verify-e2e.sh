#!/usr/bin/env bash
# lean-verify-e2e.sh — end-to-end test of the lean-verify tool.
#
# Runs the whole spec-driven workflow under the flake's lean devShell
# (flake.nix devShells.lean), executing the tool binary, not lake
# directly:
#
#   1. init    — create a project with the tool's init op
#   2. spec    — write a specification (theorem) + implementation
#   3. build   — the zero-sorry kernel gate must report GREEN
#   4. build   — with a `sorry` added, the gate must report RED
#   5. build   — sorry removed, gate GREEN again
#   6. drt     — differential random test: the built Lean model exe
#                vs a shell implementation; all inputs must match
#   7. drt     — a deliberately divergent prod command must produce a
#                reported mismatch
#   7a. drt    — a divergent prod on a script file stops the run and
#                keeps a checkpoint
#   7b. drt    — resuming with changed parameters is refused
#   7c. drt    — fixing the prod script + resume continues from the
#                first failed index and passes
#   7d. drt    — the smoke tier ("smoke":true, n=2000 quick preset)
#   7e. drt    — progress-file lifecycle: a clean run removes it
#   8a. check-inputs — all generated lines accepted by both sides
#   8b. check-inputs — a line the model side rejects is reported
#   9. translate — the charon + aeneas Rust->Lean pipeline (flake's
#                aeneas devShell): a scratch cargo crate must yield an
#                LLBC and a generated .lean carrying the Aeneas prelude
#                (guarded: skipped unless the aeneas devShell is in the
#                Nix store)
#
# Requires: nix (the flake's devShells provide the toolchains),
# jq (the assertions), and a built tool (`cargo build -p lean-verify`).
#
# Exit 0 = all assertions passed (or nix unavailable: SKIP),
# 1 = a gate failed.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug/lean-verify"
if [ ! -x "$BIN" ]; then
  BIN="$ROOT/target/release/lean-verify"
fi
if [ ! -x "$BIN" ]; then
  echo "FAIL: lean-verify binary not built (cargo build -p lean-verify)"
  exit 1
fi

if ! command -v nix >/dev/null 2>&1; then
  echo "SKIP: nix not available; cannot enter the flake's lean devShell"
  exit 0
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "SKIP: jq not available; cannot assert on tool results"
  exit 0
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/lean-verify-e2e.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# The inner script: steps 1–7. It prints one `LABEL <json>` line per
# gate result. Written with a quoted heredoc so no outer-shell
# expansion interferes; it receives WORK and BIN as arguments. It
# uses no jq (the lean devShell does not carry it): the outer shell
# does all JSON parsing.
cat > "$WORK/inner.sh" <<'INNEOF'
#!/usr/bin/env bash
set -uo pipefail
WORK="$1"
BIN="$2"

mkdir -p "$WORK/proj"
cd "$WORK/proj"

# One tool call: JSON object on stdin, JSON object on stdout.
call() { printf '%s' "$1" | "$BIN"; }

json_escape() { printf '%s' "$1" | sed 's/"/\\"/g'; }

# 1. init — the tool creates the lake project in an empty dir.
#    `lake init demo` generates: Demo.lean (the declared lib target),
#    Demo/Basic.lean, Main.lean (the declared exe root), and the exe
#    target `demo` -> .lake/build/bin/demo.
call '{"op":"init","name":"demo"}' > "$WORK/init.json"
[ -f "$WORK/proj/lakefile.toml" ] || { echo "INIT: lakefile missing"; exit 1; }
echo "INIT $(cat "$WORK/init.json")"

# 2. spec — one theorem per invariant (the reverse_concat example
#    from the spec-driven workflow), plus the DRT executable target.
cat > Demo.lean <<'LEAN'
def reverse {α : Type u} (l : List α) : List α :=
  match l with
  | [] => []
  | x :: xs => reverse xs ++ [x]

theorem reverse_concat {α : Type u} (a b : List α) :
    reverse (a ++ b) = reverse b ++ reverse a := by
  induction a with
  | nil =>
    simp [reverse]
  | cons x as ih =>
    simp [reverse, List.append_assoc, ih]
LEAN

cat > Main.lean <<'LEAN'
import Demo

def main (args : List String) : IO Unit := do
  let s <- match args with
    | a :: _ => pure a
    | [] => pure ""
  IO.print s
LEAN

# 3. build — kernel gate GREEN (no sorries anywhere).
call '{"op":"build"}' > "$WORK/green.json"
echo "GREEN $(cat "$WORK/green.json")"

# 4. build — add an unproven theorem to the DECLARED lib target
#    (Demo.lean). The gate builds every declared target, so the open
#    obligation is visible even though the exe's import graph would
#    not have reached it. Lake's incremental check is content-based:
#    the edit is detected without a sleep.
cp Demo.lean "$WORK/demo.lean.bak"
cat >> Demo.lean <<'LEAN'

theorem sorry_example {α : Type u} (a b : List α) :
    reverse (a ++ b) = reverse b ++ reverse a := by
  sorry
LEAN
call '{"op":"build"}' > "$WORK/red.json"
echo "RED $(cat "$WORK/red.json")"

# 5. build — remove the open obligation, gate GREEN again.
mv "$WORK/demo.lean.bak" Demo.lean
call '{"op":"build"}' > "$WORK/green2.json"
echo "GREEN2 $(cat "$WORK/green2.json")"

# 6. drt — the built model executable vs a shell implementation.
#    Inputs 1..32 via input_gen; the model echoes its input and the
#    production echo does the same: every input must match.
EXE="$WORK/proj/.lake/build/bin/demo"
[ -x "$EXE" ] || { echo "DRTPASS: model executable missing"; exit 1; }
M="$(json_escape "$EXE \"\$1\"")"
ECHO="$(json_escape 'printf %s "$1"')"
DIV="$(json_escape 'printf X%s "$1"')"
call "{\"op\":\"drt\",\"model\":\"$M\",\"prod\":\"$ECHO\",\"input_gen\":\"seq 1 32\",\"n\":32}" \
  > "$WORK/drt_pass.json"
echo "DRTPASS $(cat "$WORK/drt_pass.json")"

# 7. drt — prod diverges on every input: a mismatch must be reported.
call "{\"op\":\"drt\",\"model\":\"$M\",\"prod\":\"$DIV\",\"input_gen\":\"seq 1 32\",\"n\":32}" \
  > "$WORK/drt_mismatch.json"
echo "DRTMISMATCH $(cat "$WORK/drt_mismatch.json")"

# 7a. drt — a divergent prod on a SCRIPT file: the stop keeps a
#      checkpoint that a re-run can resume after the prod is fixed.
cat > "$WORK/prod.sh" <<'EOS'
#!/bin/sh
printf 'X%s' "$1"
EOS
chmod +x "$WORK/prod.sh"
P1="$(json_escape "\"$WORK/prod.sh\" \"\$1\"")"
call "{\"op\":\"drt\",\"model\":\"$M\",\"prod\":\"$P1\",\"input_gen\":\"seq 1 32\",\"n\":32}" \
  > "$WORK/drt_mismatch2.json"
echo "DRTM2 $(cat "$WORK/drt_mismatch2.json")"

# 7b. drt — resuming with changed parameters must be refused
#      (tool-level failure: exit 1, diagnostic on stderr).
BADRES="{\"op\":\"drt\",\"model\":\"$M\",\"prod\":\"$P1\",\"input_gen\":\"seq 1 32\",\"n\":31,\"resume\":true}"
RERR="$(printf '%s' "$BADRES" | "$BIN" 2>&1 >/dev/null)"
RC=$?
echo "RESUMEERR $RC $RERR"

# 7c. drt — the fix: prod.sh is "rebuilt" to echo; the same call with
#      resume:true continues from the first failed index and passes.
cat > "$WORK/prod.sh" <<'EOS'
#!/bin/sh
printf '%s' "$1"
EOS
chmod +x "$WORK/prod.sh"
call "{\"op\":\"drt\",\"model\":\"$M\",\"prod\":\"$P1\",\"input_gen\":\"seq 1 32\",\"n\":32,\"resume\":true}" \
  > "$WORK/drt_resume.json"
echo "DRTRESUME $(cat "$WORK/drt_resume.json")"

# 7d. drt — the smoke tier: "smoke":true is the quick preset (n=2000).
call "{\"op\":\"drt\",\"model\":\"$M\",\"prod\":\"$ECHO\",\"smoke\":true}" \
  > "$WORK/drt_smoke.json"
echo "DRTSMOKE $(cat "$WORK/drt_smoke.json")"

# 7e. drt — progress-file lifecycle: a clean run removes the file.
call "{\"op\":\"drt\",\"model\":\"$M\",\"prod\":\"$ECHO\",\"input_gen\":\"seq 1 32\",\"n\":32,\"progress\":true}" \
  > "$WORK/drt_prog.json"
echo "DRTPROG $(cat "$WORK/drt_prog.json")"

# 8a. check-inputs — every generated line is accepted by both sides.
call "{\"op\":\"check-inputs\",\"model\":\"$M\",\"prod\":\"$ECHO\",\"input_gen\":\"seq 1 8\",\"n\":8}" \
  > "$WORK/check_good.json"
echo "CHECKGOOD $(cat "$WORK/check_good.json")"

# 8b. check-inputs — a line the model side rejects (exit 3) is
#      reported as the first rejection; the stop keeps a checkpoint.
cat > "$WORK/reject.sh" <<'EOS'
#!/bin/sh
case "$1" in
  7) printf 'parse error: not a scenario' 1>&2; exit 3 ;;
  *) printf '%s' "$1" ;;
esac
EOS
chmod +x "$WORK/reject.sh"
R="$(json_escape "\"$WORK/reject.sh\" \"\$1\"")"
call "{\"op\":\"check-inputs\",\"model\":\"$R\",\"prod\":\"$ECHO\",\"input_gen\":\"seq 1 8\",\"n\":8,\"progress\":true}" \
  > "$WORK/check_rej.json"
echo "CHECKREJ $(cat "$WORK/check_rej.json")"
INNEOF

# Run the inner script inside the flake's lean devShell so `lake` is
# on PATH and LEAN_PATH carries the Nix-prebuilt oleans.
OUT="$(
  cd "$ROOT"
  nix develop --impure .#lean --command bash "$WORK/inner.sh" "$WORK" "$BIN" 2> "$WORK/nix.stderr"
)"
RC=$?
if [ $RC -ne 0 ]; then
  echo "FAIL: nix devShell run exited $RC"
  tail -20 "$WORK/nix.stderr"
  exit 1
fi

FAILED=0
check() {
  local name="$1" got="$2" want="$3"
  if [ "$got" != "$want" ]; then
    echo "FAIL: $name — expected $want, got '$got'"
    FAILED=1
  else
    echo "PASS: $name"
  fi
}

# The tool emits one compact JSON object per call, so each labelled
# line is exactly one line. Iterate line by line (no word splitting).
while IFS= read -r line; do
  case "$line" in
    INIT\ *)
      body="${line#INIT }"
      check "init: project created" "$(printf '%s' "$body" | jq -r '.ok')" "true"
      check "init: lake exit 0" "$(printf '%s' "$body" | jq -r '.lake_exit')" "0"
      ;;
    GREEN\ *)
      body="${line#GREEN }"
      check "build: gate GREEN" "$(printf '%s' "$body" | jq -r '.clean')" "true"
      check "build: zero sorries" "$(printf '%s' "$body" | jq -r '.sorry_count')" "0"
      ;;
    RED\ *)
      body="${line#RED }"
      check "build: sorry caught" "$(printf '%s' "$body" | jq -r '.clean')" "false"
      check "build: sorry count 1" "$(printf '%s' "$body" | jq -r '.sorry_count')" "1"
      # A sorry is a warning: lake build still exits 0.
      check "build: lake exit 0 (sorry is a warning)" "$(printf '%s' "$body" | jq -r '.lake_exit')" "0"
      ;;
    GREEN2\ *)
      check "build: clean again" "$(printf '%s' "${line#GREEN2 }" | jq -r '.clean')" "true"
      ;;
    DRTPASS\ *)
      body="${line#DRTPASS }"
      check "drt: all inputs match" "$(printf '%s' "$body" | jq -r '.pass')" "true"
      check "drt: checked 32" "$(printf '%s' "$body" | jq -r '.checked')" "32"
      ;;
    DRTMISMATCH\ *)
      body="${line#DRTMISMATCH }"
      check "drt: mismatch reported" "$(printf '%s' "$body" | jq -r '.pass')" "false"
      check "drt: stop reason mismatch" "$(printf '%s' "$body" | jq -r '.stop_reason')" "mismatch"
      check "drt: one mismatch collected" "$(printf '%s' "$body" | jq -r '(.mismatches | length)')" "1"
      ;;
    DRTM2\ *)
      body="${line#DRTM2 }"
      check "drt mismatch2: mismatch reported" "$(printf '%s' "$body" | jq -r '.pass')" "false"
      check "drt mismatch2: checkpoint kept" "$(printf '%s' "$body" | jq -r '.checkpoint')" "true"
      ;;
    RESUMEERR\ *)
      rest="${line#RESUMEERR }"
      rc="${rest%% *}"
      err="${rest#* }"
      check "resume: refused on changed n" "$rc" "1"
      case "$err" in
        *"checkpoint parameter mismatch"*) check "resume: diagnostic names the mismatch" "named" "named" ;;
        *) check "resume: diagnostic names the mismatch" "$err" "named" ;;
      esac
      ;;
    DRTRESUME\ *)
      body="${line#DRTRESUME }"
      check "drt resume: pass after fix" "$(printf '%s' "$body" | jq -r '.pass')" "true"
      check "drt resume: resumed from 0" "$(printf '%s' "$body" | jq -r '.resumed_from')" "0"
      check "drt resume: all 32 checked" "$(printf '%s' "$body" | jq -r '.checked')" "32"
      ;;
    DRTSMOKE\ *)
      body="${line#DRTSMOKE }"
      check "drt smoke: tier smoke" "$(printf '%s' "$body" | jq -r '.tier')" "smoke"
      check "drt smoke: passed" "$(printf '%s' "$body" | jq -r '.pass')" "true"
      check "drt smoke: checked 2000" "$(printf '%s' "$body" | jq -r '.checked')" "2000"
      ;;
    DRTPROG\ *)
      body="${line#DRTPROG }"
      check "drt progress: clean run removes checkpoint" "$(printf '%s' "$body" | jq -r '.checkpoint')" "false"
      ;;
    CHECKGOOD\ *)
      body="${line#CHECKGOOD }"
      check "check-inputs: ok" "$(printf '%s' "$body" | jq -r '.ok')" "true"
      check "check-inputs: checked 8" "$(printf '%s' "$body" | jq -r '.checked')" "8"
      ;;
    CHECKREJ\ *)
      body="${line#CHECKREJ }"
      check "check-inputs: not ok" "$(printf '%s' "$body" | jq -r '.ok')" "false"
      check "check-inputs: stop reason reject" "$(printf '%s' "$body" | jq -r '.stop_reason')" "reject"
      check "check-inputs: first rejected input" "$(printf '%s' "$body" | jq -r '.rejected[0].input')" "7"
      check "check-inputs: model exit 3" "$(printf '%s' "$body" | jq -r '.rejected[0].model.exit')" "3"
      ;;
    "INIT: "*)
      echo "FAIL: $line"; FAILED=1 ;;
    DRTPASS:\ *)
      echo "FAIL: $line"; FAILED=1 ;;
    "Lean 4 dev shell"*) ;;  # devShell banner, expected
    "Check the formal spec"*) ;;  # devShell banner, expected
    "") ;;
    *)
      echo "WARN: unhandled line: $line" ;;
  esac
done <<< "$OUT"

# 8. translate — the charon + aeneas Rust->Lean pipeline (the flake's
# aeneas devShell). Guarded: the aeneas devShell must already be in the
# Nix store (this e2e does not build it — that is a heavy source build).
# The tool's nix fallback enters .#aeneas itself; run from the repo root
# so the flake walk finds flake.nix.
AENEAAS_SHELL="$(nix path-info .#devShells.x86_64-linux.aeneas 2>/dev/null || true)"
if [ -z "$AENEAAS_SHELL" ] || [ ! -e "$AENEAAS_SHELL" ]; then
  echo "SKIP: aeneas devShell not in the Nix store; skipping the translate assertion"
else
  mkdir -p "$WORK/rustcrate/src"
  cat > "$WORK/rustcrate/Cargo.toml" <<'RUSTEOF'
[package]
name = "e2ecrate"
version = "0.1.0"
edition = "2021"

[dependencies]
RUSTEOF
  cat > "$WORK/rustcrate/src/lib.rs" <<'RUSTEOF'
/// Checked addition: `x + y`, or `None` on overflow.
pub fn checked_add(x: u32, y: u32) -> Option<u32> {
    if y <= u32::MAX - x {
        Some(x + y)
    } else {
        None
    }
}
RUSTEOF
  ( cd "$ROOT" && printf '%s' "{\"op\":\"translate\",\"dir\":\"$WORK/rustcrate\"}" | "$BIN" > "$WORK/translate.json" )
  TRC=$?
  echo "TRANSLATE $(cat "$WORK/translate.json" 2>/dev/null)"
  if [ $TRC -ne 0 ]; then
    echo "FAIL: translate: tool call exited $TRC"
    FAILED=1
  else
    check "translate: pipeline ok" "$(jq -r '.ok' "$WORK/translate.json")" "true"
    check "translate: llbc produced" "$(jq -r '.llbc' "$WORK/translate.json")" "e2ecrate.llbc"
    check "translate: lean generated" "$(jq -r '(.generated | length) >= 1' "$WORK/translate.json")" "true"
    GENREL="$(jq -r '.generated[0]' "$WORK/translate.json")"
    if [ -n "$GENREL" ] && [ -f "$WORK/rustcrate/$GENREL" ] \
       && grep -q "import Aeneas" "$WORK/rustcrate/$GENREL" \
       && grep -q "checked_add" "$WORK/rustcrate/$GENREL"; then
      check "translate: generated lean markers" "present" "present"
    else
      check "translate: generated lean markers" "missing" "present"
    fi
  fi
fi

# The last check-inputs run (8b) stopped on a rejection and kept its
# checkpoint; the file must still be there, owned by that run.
if [ -f "$WORK/proj/.drt-progress.json" ]; then
  check "final: check-inputs checkpoint kept" "$(jq -r '.status' "$WORK/proj/.drt-progress.json")" "reject"
else
  check "final: check-inputs checkpoint kept" "absent" "reject"
fi

if [ $FAILED -ne 0 ]; then
  echo "--- nix stderr (tail) ---"
  tail -20 "$WORK/nix.stderr"
  echo "FAIL: lean-verify e2e (assertions above)"
  exit 1
fi
echo "PASS: lean-verify e2e (init, kernel gate x3, drt pass, mismatch+resume, smoke, check-inputs, translate)"
