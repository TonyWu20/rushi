#!/usr/bin/env bash
# The session-fork e2e suite (docs/fork-design.md, issue #67).
# It drives the real `rushi`, `rushi-log`, and `assemble` binaries
# over synthetic sessions and asserts:
#   1. the fork of a live zstd log: the forked log decodes to the
#      parent's lines 1..=N, the sidecar passes the open-time
#      checks, and a later append resumes the seq numbering (P1,
#      P2, P3, P10)
#   2. the fork of a legacy plain log: the prefix stays plain text,
#      no sidecar (P2, P5)
#   3. the tool-log boundary: the last kept line keys the last
#      tool_result at or before N, and the forked context assembles
#      without a dangling tool reference (P4)
#   4. the stranded boundary: a named error names the stranded pair,
#      nothing writes, the parent log is untouched (P6)
#   5. the range guard, the existing target, and the no-partial rule:
#      named errors, no temp dirs left behind (P7, P8, P12)
#   6. the sessions root and the cwd precedence (P9, P11)
#   7. the missing parent: a named error, no directory creates (P11)
#   8. the bare dir-form parent in the CWD keeps the CWD as its
#      root (P11)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="$ROOT/target/debug"

cargo build --quiet || { echo "FAIL: cargo build"; exit 1; }

cd "$ROOT"

RUSHI="$BIN_DIR/rushi"
LOG_BIN="$BIN_DIR/rushi-log"
ASSEMBLE_BIN="$BIN_DIR/assemble"
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); }
ko() {
  FAIL=$((FAIL + 1))
  echo "FAIL: $1"
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Self-contained config so the suite is hermetic (the repo-root
# config.toml is gitignored for local use).
CONFIG="$WORK/config.toml"
cat > "$CONFIG" <<EOF
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
native_tool_paths = [
  "$ROOT/tools/bash",
  "$ROOT/tools/read",
  "$ROOT/tools/write",
  "$ROOT/tools/edit",
]

[limits]
context_budget_tokens = 8000
compact_reserve_tokens = 500
EOF

# rushi with the hermetic config and the default root.
rfork() {
  "$RUSHI" --config "$CONFIG" fork "$@"
}

# The parent log as text. The idle pass may have consolidated the
# log into a zstd stream. Sniff the magic and decode; a legacy plain
# log passes through.
slog() {
  local f=$1
  if [ -f "$f" ] && head -c 4 "$f" 2>/dev/null | od -An -v -tx1 | head -1 | rg -q '28 b5 2f fd'; then
    zstd -dc "$f" 2>/dev/null || true
  else
    [ -f "$f" ] && cat "$f"
  fi
}

is_zstd() {
  [ -f "$1" ] && head -c 4 "$1" 2>/dev/null | od -An -v -tx1 | head -1 | rg -q '28 b5 2f fd'
}

# One 32-byte index record check. Args: log, expected record count,
# and (for the last record) the expected seq start. Exits 0 when the
# sidecar passes the open-time structural checks (the same checks
# `open_indexed` runs: seq from 1, contiguous frame ranges, the
# last record covering the stream end, the magic at each offset).
idx_check() {
  local log=$1 want_records=$2 want_seq=$3
  python3 - "$log" "$want_records" "$want_seq" <<'PY'
import struct, subprocess, sys
log, want_records, want_seq = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
data = open(log, 'rb').read()
assert data[:4] == b'\x28\xb5\x2f\xfd', "the log is a zstd stream"
recs = open(log + '.idx', 'rb').read()
assert len(recs) % 32 == 0, "the sidecar holds whole records"
n = len(recs) // 32
assert n == want_records, f"record count {n} != {want_records}"
last_off = last_len = prev_seq = 0
for i in range(n):
    seq, off, flen, tlen = struct.unpack('<' + 'Q' * 4, recs[32 * i:32 * (i + 1)])
    if i == 0:
        assert seq == 1, "the seq runs from 1"
    else:
        assert off == last_off + last_len, "the frame ranges are contiguous"
        assert seq > prev_seq, "the seq steps are strictly increasing"
    assert data[off:off + 4] == b'\x28\xb5\x2f\xfd', "the magic sits at each offset"
    last_off, last_len, prev_seq = off, flen, seq
assert seq == want_seq, f"last record seq {seq} != {want_seq}"
assert last_off + last_len == len(data), "the last record covers the stream end"
if last_off == 0:
    decoded = subprocess.run(['zstd', '-dc', log], capture_output=True,
                             check=True).stdout
    assert tlen == len(decoded), "text_len is the decoded byte length"
else:
    assert tlen > 0, "the resume record carries its own range bytes"
PY
}

# ── 1. the fork of a live zstd log ───────────────────────────────

SESS="$WORK/sessions"
mkdir -p "$SESS/zstd-p"
for ev in \
  '{"v":1,"type":"user_message","ts":"t","content":"task"}' \
  '{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c1","name":"bash","arguments":{"command":"a"}}],"stop_reason":"tool_calls"}' \
  '{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"text":"A result"},"is_error":false}' \
  '{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c2","name":"bash","arguments":{"command":"b"}}],"stop_reason":"tool_calls"}' \
  '{"v":1,"type":"tool_result","ts":"t","id":"c2","value":{"text":"B result"},"is_error":false}'
do
  printf '%s\n' "$ev" | "$LOG_BIN" --session "$SESS/zstd-p" || ko "1: the parent append failed"
done
if is_zstd "$SESS/zstd-p/events.jsonl"; then
  ok "1: the parent log is a live zstd stream"
else
  ko "1: the parent log is not a zstd stream"
fi

if rfork zstd-p zf --fork-at 3 --sessions-root "$SESS"; then
  ok "1: the settled fork exits 0"
else
  ko "1: the settled fork refused"
fi

ZF="$SESS/zf"
if diff <(slog "$SESS/zstd-p/events.jsonl" | head -n 3) \
      <(slog "$ZF/events.jsonl" 2>/dev/null) > /dev/null; then
  ok "1: the forked log decodes to the parent's lines 1..=3"
else
  ko "1: the forked log content is wrong"
fi

if is_zstd "$ZF/events.jsonl" && idx_check "$ZF/events.jsonl" 1 1; then
  ok "1: the forked log is one consolidated frame with a valid sidecar"
else
  ko "1: the forked zstd layout or sidecar is wrong"
fi

# P10: the forked session is idle. It holds no runtime files.
RUNTIME_FILES=""
for name in loop.pid loop.meta .loop.lock; do
  [ -e "$ZF/$name" ] && RUNTIME_FILES="$RUNTIME_FILES $name"
done
if [ -z "$RUNTIME_FILES" ]; then
  ok "1: the forked session holds no runtime files"
else
  ko "1: the forked session holds runtime files:$RUNTIME_FILES"
fi

# 1b. the resume: the next append lands after the forked frame.
printf '%s\n' '{"v":1,"type":"user_message","ts":"t","content":"more"}' |
  "$LOG_BIN" --session "$ZF" || ko "1b: the resume append failed"
RESUME_LINES=$(slog "$ZF/events.jsonl" | wc -l | tr -d ' ')
if [ "$RESUME_LINES" = "4" ]; then
  ok "1b: the resumed fork decodes to the prefix plus the new line"
else
  ko "1b: the resumed fork holds $RESUME_LINES line(s)"
fi
if idx_check "$ZF/events.jsonl" 2 4; then
  ok "1b: the resume record continues the seq numbering"
else
  ko "1b: the resume record is wrong"
fi

# ── 2. the fork of a legacy plain log ────────────────────────────

mkdir -p "$SESS/plain-p"
cat > "$SESS/plain-p/events.jsonl" <<'EOF'
{"v":1,"type":"user_message","ts":"t","content":"task"}
{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c1","name":"bash","arguments":{"command":"a"}}],"stop_reason":"tool_calls"}
{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"text":"A result"},"is_error":false}
{"v":1,"type":"user_message","ts":"t","content":"more"}
EOF
cat > "$SESS/plain-p/tools.jsonl" <<'EOF'
{"v":1,"ts":"t","id":"c1","exit":0,"stdout":"FULL-BODY-1","stderr":"","text":"FULL-BODY-1","is_error":false}
EOF

if rfork plain-p pf --fork-at 3 --sessions-root "$SESS"; then
  ok "2: the plain fork exits 0"
else
  ko "2: the plain fork refused"
fi
ZF2="$SESS/pf"
if ! is_zstd "$ZF2/events.jsonl"; then
  ok "2: the forked log stays plain text"
else
  ko "2: the forked log became a zstd stream"
fi
if [ ! -e "$ZF2/events.jsonl.idx" ]; then
  ok "2: the plain fork holds no sidecar"
else
  ko "2: the plain fork holds a sidecar"
fi
if diff <(head -n 3 "$SESS/plain-p/events.jsonl") \
      <(cat "$ZF2/events.jsonl") > /dev/null; then
  ok "2: the plain forked log decodes to the parent's lines 1..=3"
else
  ko "2: the plain forked log content is wrong"
fi
# P5: no tool_result at or before the boundary... the boundary here
# is 3, a settled result. The tool log keeps the c1 line (P4).
if [ "$(cat "$ZF2/tools.jsonl" | wc -l | tr -d ' ')" = "1" ] &&
   grep -q '"id":"c1"' "$ZF2/tools.jsonl"; then
  ok "2: the tool log keeps the c1 record, the cut line"
else
  ko "2: the tool log cut is wrong"
fi
# A later append to a plain log stays plain (the format is decided
# at creation and held for the file's life).
printf '%s\n' '{"v":1,"type":"user_message","ts":"t","content":"later"}' |
  "$LOG_BIN" --session "$ZF2" || ko "2: the plain append failed"
if ! is_zstd "$ZF2/events.jsonl"; then
  ok "2: the plain fork stays plain after the append"
else
  ko "2: the plain fork became zstd after the append"
fi

# ── 3. the tool-log boundary assembles without a dangling ref ────

mkdir -p "$SESS/asm-p"
cat > "$SESS/asm-p/events.jsonl" <<'EOF'
{"v":1,"type":"user_message","ts":"t","content":"task"}
{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c1","name":"bash","arguments":{"command":"a"}}],"stop_reason":"tool_calls"}
{"v":1,"type":"tool_result","ts":"t","id":"c1","value":{"text":"P1","details":null},"is_error":false,"bytes":4,"tool_log":"tools.jsonl"}
{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c2","name":"bash","arguments":{"command":"b"}}],"stop_reason":"tool_calls"}
{"v":1,"type":"tool_result","ts":"t","id":"c2","value":{"text":"P2","details":null},"is_error":false,"bytes":4,"tool_log":"tools.jsonl"}
EOF
cat > "$SESS/asm-p/tools.jsonl" <<'EOF'
{"v":1,"ts":"t","id":"c1","exit":0,"stdout":"FULL-BODY-1","stderr":"","text":"FULL-BODY-1","is_error":false}
{"v":1,"ts":"t","id":"c2","exit":0,"stdout":"FULL-BODY-2","stderr":"","text":"FULL-BODY-2","is_error":false}
EOF

if rfork asm-p af --fork-at 3 --sessions-root "$SESS"; then
  ok "3: the tool-boundary fork exits 0"
else
  ko "3: the tool-boundary fork refused"
fi
ZF3="$SESS/af"
if [ "$(cat "$ZF3/tools.jsonl" | wc -l | tr -d ' ')" = "1" ] &&
   grep -q '"id":"c1"' "$ZF3/tools.jsonl" &&
   ! grep -q '"id":"c2"' "$ZF3/tools.jsonl"; then
  ok "3: the last kept tool line keys the last tool_result at or before N"
else
  ko "3: the tool boundary is wrong: $(cat "$ZF3/tools.jsonl")"
fi
ASM_OUT="$WORK/asm.out"
"$ASSEMBLE_BIN" --session "$ZF3" --config "$CONFIG" >"$ASM_OUT" 2>"$WORK/asm.err"
ASM_RC=$?
if [ "$ASM_RC" -eq 0 ]; then
  ok "3: the forked context assembles"
else
  ko "3: the assemble failed: $(cat "$WORK/asm.err")"
fi
if grep -q "FULL-BODY-1" "$ASM_OUT" && ! grep -q "FULL-BODY-2" "$ASM_OUT"; then
  ok "3: the kept body rides the input, the cut body stays out"
else
  ko "3: the tool log reference is dangling or leaked"
fi

# ── 4. the stranded boundary ─────────────────────────────────────

mkdir -p "$SESS/str-p"
cat > "$SESS/str-p/events.jsonl" <<'EOF'
{"v":1,"type":"user_message","ts":"t","content":"task"}
{"v":1,"type":"assistant_message","ts":"t","content":"","tool_calls":[{"id":"c9","name":"bash","arguments":{"command":"x"}}],"stop_reason":"tool_calls"}
EOF
STR_HASH_BEFORE=$(sha256sum "$SESS/str-p/events.jsonl" | cut -d' ' -f1)
ERR4="$WORK/str.err"
if rfork str-p sf --fork-at 2 --sessions-root "$SESS" 2>"$ERR4" >/dev/null; then
  ko "4: the stranded fork must be refused"
else
  if grep -q "strands tool call c9" "$ERR4"; then
    ok "4: the refusal names the stranded pair"
  else
    ko "4: the stderr does not name the stranded pair: $(cat "$ERR4")"
  fi
fi
if [ ! -e "$SESS/sf" ]; then
  ok "4: the refusal writes nothing"
else
  ko "4: the refusal left the target dir"
fi
STR_HASH_AFTER=$(sha256sum "$SESS/str-p/events.jsonl" | cut -d' ' -f1)
if [ "$STR_HASH_BEFORE" = "$STR_HASH_AFTER" ]; then
  ok "4: the parent log is untouched"
else
  ko "4: the refused fork touched the parent log"
fi

# ── 5. the range guard, the existing target, no partial writes ───

mkdir -p "$SESS/rng-p"
cat > "$SESS/rng-p/events.jsonl" <<'EOF'
{"v":1,"type":"user_message","ts":"t","content":"task"}
{"v":1,"type":"user_message","ts":"t","content":"more"}
{"v":1,"type":"user_message","ts":"t","content":"last"}
EOF
ERR5="$WORK/rng.err"
if rfork rng-p rf --fork-at 99 --sessions-root "$SESS" 2>"$ERR5" >/dev/null; then
  ko "5: the out-of-range fork must be refused"
else
  if grep -q "out of range" "$ERR5"; then
    ok "5: the out-of-range boundary is refused by name"
  else
    ko "5: the stderr does not name the range: $(cat "$ERR5")"
  fi
fi
if [ ! -e "$SESS/rf" ]; then
  ok "5: the out-of-range refusal writes nothing"
else
  ko "5: the out-of-range refusal left the target dir"
fi
# A zero boundary is a CLI-level named refusal (the seq is 1-based).
ERR5B="$WORK/rng0.err"
if rfork rng-p rf0 --fork-at 0 --sessions-root "$SESS" 2>"$ERR5B" >/dev/null; then
  ko "5: the zero boundary must be refused"
else
  ok "5: the zero boundary is refused by the CLI"
fi
# The existing target refuses. The session stays untouched.
mkdir -p "$SESS/xf"
echo "keep" > "$SESS/xf/events.jsonl"
ERR5C="$WORK/exists.err"
if rfork rng-p xf --fork-at 3 --sessions-root "$SESS" 2>"$ERR5C" >/dev/null; then
  ko "5: the existing target must be refused"
else
  if grep -q "already exists" "$ERR5C"; then
    ok "5: the existing target is refused by name"
  else
    ko "5: the stderr does not name the target: $(cat "$ERR5C")"
  fi
fi
if [ "$(cat "$SESS/xf/events.jsonl")" = "keep" ]; then
  ok "5: the existing session is untouched"
else
  ko "5: the existing session was modified"
fi
# No partial writes: no temp dirs or temp files under the root.
STRAYS=$(ls -a "$SESS" | grep -E '^\.[a-z0-9]+\.fork-[0-9]+$' | wc -l | tr -d ' ')
if [ "$STRAYS" = "0" ]; then
  ok "5: no temp dirs survive under the root"
else
  ko "5: temp dirs survived under the root: $STRAYS"
fi

# ── 6. the sessions root and the cwd precedence ──────────────────

mkdir -p "$SESS/p6"
cat > "$SESS/p6/events.jsonl" <<'EOF'
{"v":1,"type":"user_message","ts":"t","content":"task"}
EOF
echo "/work/parent" > "$SESS/p6/cwd"

if rfork p6 f6a --fork-at 1 --sessions-root "$SESS"; then
  if [ "$(cat "$SESS/f6a/cwd")" = "/work/parent" ]; then
    ok "6: without --cwd the fork copies the parent's recorded cwd"
  else
    ko "6: the recorded cwd was not copied: $(cat "$SESS/f6a/cwd" 2>/dev/null)"
  fi
else
  ko "6: the record-copy fork refused"
fi
mkdir -p "$WORK/work6"
if rfork p6 f6b --fork-at 1 --sessions-root "$SESS" --cwd "$WORK/work6"; then
  if [ "$(cat "$SESS/f6b/cwd")" = "$WORK/work6" ]; then
    ok "6: an explicit --cwd wins over the recorded cwd"
  else
    ko "6: the flag cwd did not win: $(cat "$SESS/f6b/cwd" 2>/dev/null)"
  fi
else
  ko "6: the flag-cwd fork refused"
fi
# No record, no flag: the fork holds no cwd file.
mkdir -p "$SESS/p6n"
echo '{"v":1,"type":"user_message","ts":"t","content":"task"}' > "$SESS/p6n/events.jsonl"
if rfork p6n f6c --fork-at 1 --sessions-root "$SESS"; then
  if [ ! -e "$SESS/f6c/cwd" ]; then
    ok "6: with no record and no flag the fork holds no cwd file"
  else
    ko "6: the fork held a cwd file: $(cat "$SESS/f6c/cwd" 2>/dev/null)"
  fi
else
  ko "6: the no-record fork refused"
fi
# The dir-form parent keeps its own root without the flag.
if rfork "$SESS/p6" f6d --fork-at 1; then
  if [ -e "$SESS/f6d/events.jsonl" ]; then
    ok "6: the dir-form parent lands under its own root"
  else
    ko "6: the dir-form fork missed its own root"
  fi
else
  ko "6: the dir-form fork refused"
fi

# ── 7. the missing parent ────────────────────────────────────────

ERR7="$WORK/ghost.err"
if rfork ghost f7 --fork-at 1 --sessions-root "$SESS" 2>"$ERR7" >/dev/null; then
  ko "7: the missing parent must be refused"
else
  if grep -q "was not found" "$ERR7"; then
    ok "7: the missing parent is refused by name"
  else
    ko "7: the stderr does not name the parent: $(cat "$ERR7")"
  fi
fi
if [ ! -e "$SESS/ghost" ] && [ ! -e "$SESS/f7" ]; then
  ok "7: the missing parent creates no directory"
else
  ko "7: the missing parent created a directory"
fi

# ── 8. the bare dir-form parent in the CWD keeps the CWD root ───

mkdir -p "$WORK/relroot/p"
cat > "$WORK/relroot/p/events.jsonl" <<'EVENTS'
{"v":1,"type":"user_message","ts":"t","content":"task"}
EVENTS

# The parent is a bare name in the CWD (no separator, no flag):
# its own root is the CWD, not the config sessions root (P11).
if ( cd "$WORK/relroot" && rfork p relf --fork-at 1 ) &&
   [ -e "$WORK/relroot/relf/events.jsonl" ]; then
  ok "8: the bare dir-form parent keeps the CWD root"
else
  ko "8: the bare dir-form fork missed the CWD root"
fi

echo
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
