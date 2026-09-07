#!/usr/bin/env bash
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TOOLS_DIR="$(cd "$SCRIPT_DIR/../tools" && pwd)"
TEST_DIR=$(mktemp -d "${TMPDIR:-/tmp}/conformance.XXXXXX")
trap 'rm -rf "$TEST_DIR"' EXIT

# Resolve tool binary paths
READ_BIN="$TOOLS_DIR/read/bin/read"
WRITE_BIN="$TOOLS_DIR/write/bin/write"
EDIT_BIN="$TOOLS_DIR/edit/bin/edit"
BASH_BIN="$TOOLS_DIR/bash/bin/harness-bash"

# Fallback to target/debug if bin/ not found
if [ ! -f "$READ_BIN" ]; then
  READ_BIN="$(cd "$SCRIPT_DIR/../target/debug" && pwd)/read"
fi
if [ ! -f "$WRITE_BIN" ]; then
  WRITE_BIN="$(cd "$SCRIPT_DIR/../target/debug" && pwd)/write"
fi
if [ ! -f "$EDIT_BIN" ]; then
  EDIT_BIN="$(cd "$SCRIPT_DIR/../target/debug" && pwd)/edit"
fi
if [ ! -f "$BASH_BIN" ]; then
  BASH_BIN="$(cd "$SCRIPT_DIR/../target/debug" && pwd)/harness-bash"
fi

PASSED=0
FAILED=0

run_test() {
  local name="$1"
  local expected_exit="$2"
  local expected_stderr_contains="${3:-}"
  local expected_stdout_contains="${4:-}"
  local input="${5:-}"
  local tool_path="${6:-}"

  local actual_exit=0
  local stdout=""
  local stderr=""

  if [ -n "$input" ]; then
    stdout=$(printf '%s' "$input" | "$tool_path" 2>"$TEST_DIR/stderr")
    actual_exit=$?
    stderr=$(cat "$TEST_DIR/stderr" 2>/dev/null || true)
  else
    "$tool_path" < /dev/null > "$TEST_DIR/stdout" 2>"$TEST_DIR/stderr"
    actual_exit=$?
    stdout=$(cat "$TEST_DIR/stdout" 2>/dev/null || true)
    stderr=$(cat "$TEST_DIR/stderr" 2>/dev/null || true)
  fi

  if [ "$actual_exit" -ne "$expected_exit" ]; then
    echo "FAIL: $name - expected exit $expected_exit, got $actual_exit"
    FAILED=$((FAILED + 1))
    return
  fi

  if [ -n "$expected_stderr_contains" ]; then
    if ! printf '%s' "$stderr" | rg --fixed-strings "$expected_stderr_contains" > /dev/null 2>&1; then
      echo "FAIL: $name - expected stderr to contain '$expected_stderr_contains', got: $stderr"
      FAILED=$((FAILED + 1))
      return
    fi
  fi

  if [ -n "$expected_stdout_contains" ]; then
    if ! printf '%s' "$stdout" | rg --fixed-strings "$expected_stdout_contains" > /dev/null 2>&1; then
      echo "FAIL: $name - expected stdout to contain '$expected_stdout_contains', got: $stdout"
      FAILED=$((FAILED + 1))
      return
    fi
  fi

  echo "PASS: $name"
  PASSED=$((PASSED + 1))
}

# Extended runner for the bash tool. Adds JSON shape checks, env control, and
# cwd control on top of the exit-code checks in run_test.
#
#   run_bash_test <name> <expected_exit> <input_json> <env_spec> <cwd> [check ...]
#
#   expected_exit: "0" for a command-level result (tool exits 0), "nonzero"
#                  for a tool-level failure (tool exits non-zero).
#   env_spec:      space-separated KEY=VALUE assignments, or empty.
#   cwd:           directory to run the tool in, or empty.
#
# Each check is one of:
#   ec:<n>            exit_code field equals <n>
#   text_contains:s   text field contains s
#   stdout_contains:s stdout field contains s
#   stdout_equals:v   stdout field equals v (trailing newline stripped)
#   stdout_empty      stdout field is empty
#   stderr_contains:s stderr field contains s
#   timed_out:b       timed_out field equals b
#   truncated:b       truncated field equals b
#   marker_line2      the truncation marker is the second line of text
#   stderr_diag       stderr diagnostic present (tool-level failure)
#   no_survive:re     no process matching re survives after the tool returns
run_bash_test() {
  local name="$1"
  local expected_exit="$2"
  local input="$3"
  local env_spec="${4:-}"
  local cwd="${5:-}"
  shift 5
  local checks=("$@")

  local tmp_out="$TEST_DIR/bash_out.$$"
  local tmp_err="$TEST_DIR/bash_err.$$"
  local actual_exit=0

  if [ -n "$env_spec" ]; then
    # shellcheck disable=SC2206
    local envs=($env_spec)
    if [ -n "$cwd" ]; then
      ( cd "$cwd" && env "${envs[@]}" "$BASH_BIN" > "$tmp_out" 2> "$tmp_err" <<< "$input" )
      actual_exit=$?
    else
      env "${envs[@]}" "$BASH_BIN" > "$tmp_out" 2> "$tmp_err" <<< "$input"
      actual_exit=$?
    fi
  else
    if [ -n "$cwd" ]; then
      ( cd "$cwd" && "$BASH_BIN" > "$tmp_out" 2> "$tmp_err" <<< "$input" )
      actual_exit=$?
    else
      "$BASH_BIN" > "$tmp_out" 2> "$tmp_err" <<< "$input"
      actual_exit=$?
    fi
  fi

  local stdout stderr
  stdout=$(cat "$tmp_out" 2>/dev/null || true)
  stderr=$(cat "$tmp_err" 2>/dev/null || true)

  if [ "$expected_exit" = "0" ]; then
    if [ "$actual_exit" -ne 0 ]; then
      echo "FAIL: $name - expected tool exit 0, got $actual_exit (stderr: $stderr)"
      FAILED=$((FAILED + 1)); return
    fi
    if [ -n "$stderr" ]; then
      echo "FAIL: $name - expected empty stderr, got: $stderr"
      FAILED=$((FAILED + 1)); return
    fi
    if ! printf '%s' "$stdout" | jq -e 'type=="object" and has("text")' > /dev/null 2>&1; then
      echo "FAIL: $name - stdout is not a JSON object with a text field"
      FAILED=$((FAILED + 1)); return
    fi
  else
    if [ "$actual_exit" -eq 0 ]; then
      echo "FAIL: $name - expected non-zero tool exit, got 0"
      FAILED=$((FAILED + 1)); return
    fi
  fi

  local c
  for c in "${checks[@]}"; do
    case "$c" in
      ec:*)
        local want="${c#ec:}"
        local got=$(printf '%s' "$stdout" | jq -r '.exit_code' 2>/dev/null)
        if [ "$got" != "$want" ]; then
          echo "FAIL: $name - exit_code: expected $want, got $got"
          FAILED=$((FAILED + 1)); return
        fi
        ;;
      text_contains:*)
        local want="${c#text_contains:}"
        if ! printf '%s' "$stdout" | jq -r '.text' 2>/dev/null | rg --fixed-strings -q "$want"; then
          echo "FAIL: $name - text does not contain '$want'"
          FAILED=$((FAILED + 1)); return
        fi
        ;;
      stdout_contains:*)
        local want="${c#stdout_contains:}"
        if ! printf '%s' "$stdout" | jq -r '.stdout' 2>/dev/null | rg --fixed-strings -q "$want"; then
          echo "FAIL: $name - stdout field does not contain '$want'"
          FAILED=$((FAILED + 1)); return
        fi
        ;;
      stdout_equals:*)
        local want="${c#stdout_equals:}"
        local got=$(printf '%s' "$stdout" | jq -r '.stdout' 2>/dev/null)
        if [ "$got" != "$want" ]; then
          echo "FAIL: $name - stdout field: expected '$want', got '$got'"
          FAILED=$((FAILED + 1)); return
        fi
        ;;
      stdout_empty)
        local got=$(printf '%s' "$stdout" | jq -r '.stdout' 2>/dev/null)
        if [ -n "$got" ]; then
          echo "FAIL: $name - expected empty stdout, got: $got"
          FAILED=$((FAILED + 1)); return
        fi
        ;;
      stderr_contains:*)
        local want="${c#stderr_contains:}"
        if ! printf '%s' "$stdout" | jq -r '.stderr' 2>/dev/null | rg --fixed-strings -q "$want"; then
          echo "FAIL: $name - stderr field does not contain '$want'"
          FAILED=$((FAILED + 1)); return
        fi
        ;;
      timed_out:*)
        local want="${c#timed_out:}"
        local got=$(printf '%s' "$stdout" | jq -r '.timed_out' 2>/dev/null)
        if [ "$got" != "$want" ]; then
          echo "FAIL: $name - timed_out: expected $want, got $got"
          FAILED=$((FAILED + 1)); return
        fi
        ;;
      truncated:*)
        local want="${c#truncated:}"
        local got=$(printf '%s' "$stdout" | jq -r '.truncated' 2>/dev/null)
        if [ "$got" != "$want" ]; then
          echo "FAIL: $name - truncated: expected $want, got $got"
          FAILED=$((FAILED + 1)); return
        fi
        ;;
      marker_line2)
        local line2=$(printf '%s' "$stdout" | jq -r '.text' 2>/dev/null | sed -n '2p')
        if ! printf '%s' "$line2" | rg -q '^\[output truncated:'; then
          echo "FAIL: $name - marker is not the second line of text (line 2: $line2)"
          FAILED=$((FAILED + 1)); return
        fi
        ;;
      stderr_diag)
        if [ -z "$stderr" ]; then
          echo "FAIL: $name - expected a stderr diagnostic, got none"
          FAILED=$((FAILED + 1)); return
        fi
        ;;
      no_survive:*)
        local re="${c#no_survive:}"
        sleep 0.2
        if pgrep -f "$re" > /dev/null 2>&1; then
          echo "FAIL: $name - process matching '$re' survived the tool"
          FAILED=$((FAILED + 1)); return
        fi
        ;;
      *)
        echo "FAIL: $name - unknown check '$c'"
        FAILED=$((FAILED + 1)); return
        ;;
    esac
  done

  echo "PASS: $name"
  PASSED=$((PASSED + 1))
}

# Read tool tests
echo "=== Read Tool Tests ==="

# Create test files
printf 'line1\nline2\nline3\n' > "$TEST_DIR/small.txt"
printf '' > "$TEST_DIR/empty.txt"
printf 'line1\nline2\nline3\n' > "$TEST_DIR/offset_test.txt"
printf '%0.s-' $(seq 1 2500) > "$TEST_DIR/long_line.txt"; printf '\n' >> "$TEST_DIR/long_line.txt"
printf '\x00\x01\x02\x03' > "$TEST_DIR/binary.bin"

# Small file test
run_test "read: small file" 0 "" "line1" '{"file_path":"'"$TEST_DIR/small.txt"'"}' "$READ_BIN"

# Empty file test
run_test "read: empty file" 0 "" "total 0 lines" '{"file_path":"'"$TEST_DIR/empty.txt"'"}' "$READ_BIN"

# Offset/limit pagination test
run_test "read: offset/limit pagination" 0 "" "line2" '{"file_path":"'"$TEST_DIR/small.txt"'","offset":2,"limit":1}' "$READ_BIN"

# Offset past EOF test
run_test "read: offset past EOF" 1 "past end of file" "" '{"file_path":"'"$TEST_DIR/small.txt"'","offset":100}' "$READ_BIN"

# Long line truncation test
run_test "read: long line truncation" 0 "" "line truncated" '{"file_path":"'"$TEST_DIR/long_line.txt"'"}' "$READ_BIN"

# Binary file test
run_test "read: binary file" 1 "not a UTF-8 text file" "" '{"file_path":"'"$TEST_DIR/binary.bin"'"}' "$READ_BIN"

# File not found test
run_test "read: file not found" 1 "file not found" "" '{"file_path":"'"$TEST_DIR/nonexistent_file.txt"'"}' "$READ_BIN"

# 60KB file byte-cap test: 40 lines of 2000 chars each (about 80KB)
for _i in $(seq 1 40); do printf 'y%.0s' $(seq 1 2000); printf '\n'; done > "$TEST_DIR/big60k.txt"
run_test "read: 60KB byte cap" 0 "" "lines omitted" '{"file_path":"'"$TEST_DIR/big60k.txt"'"}' "$READ_BIN"

# Write tool tests
echo ""
echo "=== Write Tool Tests ==="

run_test "write: create new file" 0 "" "Successfully wrote" '{"file_path":"'"$TEST_DIR/new_file.txt"'","content":"hello world"}' "$WRITE_BIN"

# Overwrite existing file test
run_test "write: overwrite existing file" 0 "" "Successfully wrote" '{"file_path":"'"$TEST_DIR/new_file.txt"'","content":"new content"}' "$WRITE_BIN"

# Create parent dirs test
run_test "write: create parent dirs" 0 "" "Successfully wrote" '{"file_path":"'"$TEST_DIR/subdir/deep/file.txt"'","content":"deep content"}' "$WRITE_BIN"

# Content too large test: generate 2MB of X chars as JSON input
(
  printf '{"file_path":"%s/big.txt","content":"' "$TEST_DIR"
  dd if=/dev/zero bs=1024 count=2048 2>/dev/null | tr '\0' 'X'
  printf '"}'
) > "$TEST_DIR/big_input.json"
run_test "write: content too large" 1 "exceeds max" '' "$(cat "$TEST_DIR/big_input.json")" "$WRITE_BIN"

# Edit tool tests
echo ""
echo "=== Edit Tool Tests ==="

printf 'hello world\n' > "$TEST_DIR/edit_test.txt"
run_test "edit: unique match success" 0 "" "updated successfully" '{"file_path":"'"$TEST_DIR/edit_test.txt"'","old_string":"hello","new_string":"goodbye"}' "$EDIT_BIN"

# No match error test
printf 'hello world\n' > "$TEST_DIR/edit_nomatch.txt"
run_test "edit: no match error" 1 "old_string not found" '' '{"file_path":"'"$TEST_DIR/edit_nomatch.txt"'","old_string":"xyz","new_string":"abc"}' "$EDIT_BIN"

# Multiple matches error test
printf 'hello\nhello\n' > "$TEST_DIR/edit_multi.txt"
run_test "edit: multiple matches error" 1 "appears 2 times" '' '{"file_path":"'"$TEST_DIR/edit_multi.txt"'","old_string":"hello","new_string":"hi"}' "$EDIT_BIN"

# replace_all success test
printf 'hello\nhello\n' > "$TEST_DIR/edit_replace_all.txt"
run_test "edit: replace_all success" 0 "" "updated successfully" '{"file_path":"'"$TEST_DIR/edit_replace_all.txt"'","old_string":"hello","new_string":"hi","replace_all":true}' "$EDIT_BIN"

# Empty old_string error test
run_test "edit: empty old_string error" 1 "must not be empty" '' '{"file_path":"'"$TEST_DIR/edit_test.txt"'","old_string":"","new_string":"hi"}' "$EDIT_BIN"

# Identical strings error test
run_test "edit: identical strings error" 1 "identical" '' '{"file_path":"'"$TEST_DIR/edit_test.txt"'","old_string":"goodbye","new_string":"goodbye"}' "$EDIT_BIN"

# Empty new_string deletion test
printf 'hello world\n' > "$TEST_DIR/edit_delete.txt"
run_test "edit: empty new_string deletion" 0 "" "updated successfully" '{"file_path":"'"$TEST_DIR/edit_delete.txt"'","old_string":"hello ","new_string":""}' "$EDIT_BIN"

# CRLF preservation test
printf 'hello\r\nworld\r\n' > "$TEST_DIR/edit_crlf.txt"
run_test "edit: crlf preservation" 0 "" "updated successfully" '{"file_path":"'"$TEST_DIR/edit_crlf.txt"'","old_string":"hello","new_string":"hi"}' "$EDIT_BIN"

# CRLF match with LF old_string test
run_test "edit: crlf match with lf old_string" 0 "" "updated successfully" '{"file_path":"'"$TEST_DIR/edit_crlf.txt"'","old_string":"world","new_string":"there"}' "$EDIT_BIN"

# Missing file error test
run_test "edit: missing file error" 1 "file not found" '' '{"file_path":"'"$TEST_DIR/nonexistent_edit.txt"'","old_string":"hello","new_string":"hi"}' "$EDIT_BIN"

# Binary file error test
printf '\x00\x01\x02' > "$TEST_DIR/edit_binary.bin"
run_test "edit: binary file error" 1 "not a UTF-8 text file" '' '{"file_path":"'"$TEST_DIR/edit_binary.bin"'","old_string":"x","new_string":"y"}' "$EDIT_BIN"

# FT-004: a multibyte character straddling byte 8192 must not panic.
# 8191 ASCII bytes, then U+2500 '─' at bytes 8191..8194 (straddles 8192).
python3 - "$TEST_DIR/edit_ft004.txt" <<'PY'
import sys
with open(sys.argv[1], "w", encoding="utf-8") as f:
    f.write("a" * 8191)
    f.write("\u2500")
    f.write(" tail-marker\n")
PY
run_test "edit: multibyte char straddling byte 8192 (FT-004)" 0 "" "updated successfully" '{"file_path":"'"$TEST_DIR/edit_ft004.txt"'","old_string":"tail-marker","new_string":"tail-edited"}' "$EDIT_BIN"

# BOM preservation test: verify first 3 bytes stay 0xEF 0xBB 0xBF after edit
printf '\xEF\xBB\xBFhello\n' > "$TEST_DIR/edit_bom.txt"
BOM_STDOUT=$(printf '%s' '{"file_path":"'"$TEST_DIR/edit_bom.txt"'","old_string":"hello","new_string":"hi"}' | "$EDIT_BIN" 2>/dev/null)
if printf '%s' "$BOM_STDOUT" | rg --fixed-strings "updated successfully" > /dev/null 2>&1; then
  FIRST3=$(head -c 3 "$TEST_DIR/edit_bom.txt" | xxd -p)
  if [ "$FIRST3" = "efbbbf" ]; then
    echo "PASS: edit: BOM preservation"
    PASSED=$((PASSED + 1))
  else
    echo "FAIL: edit: BOM preservation - BOM bytes changed to $FIRST3"
    FAILED=$((FAILED + 1))
  fi
else
  echo "FAIL: edit: BOM preservation - edit failed: $BOM_STDOUT"
  FAILED=$((FAILED + 1))
fi

echo ""

echo "=== Bash Tool Tests ==="

# bash: echo - exit 0, text contains hello, exit_code 0
run_bash_test "bash: echo" 0 '{"command":"echo hello"}' '' '' 'ec:0' 'text_contains:hello'

# bash: non-zero exit - tool exit 0, exit_code 42, not an error
run_bash_test "bash: non-zero exit" 0 '{"command":"exit 42"}' '' '' 'ec:42'

# bash: command not found - a command-level result, exit_code 127, not an error
run_bash_test "bash: command not found" 0 '{"command":"nonexistent_cmd_xyz"}' '' '' 'ec:127'

# bash: stderr - stderr field is populated
run_bash_test "bash: stderr" 0 '{"command":"echo err >&2"}' '' '' 'stderr_contains:err'

# bash: combined output - both stdout and stderr populated
run_bash_test "bash: combined output" 0 '{"command":"echo out; echo err >&2"}' '' '' \
  'stdout_contains:out' 'stderr_contains:err'

# bash: timeout - timed_out true, exit_code 143, tool exit 0, group killed
run_bash_test "bash: timeout" 0 '{"command":"sleep 10","timeout_secs":2}' '' '' \
  'ec:143' 'timed_out:true' 'no_survive:^sleep 10$'

# bash: output cap - truncated true, marker is the second line of text
cap_input=$(cat <<'EOF'
{"command":"head -c 100000 /dev/zero | tr '\\0' 'a'"}
EOF
)
run_bash_test "bash: output cap" 0 "$cap_input" '' '' 'truncated:true' 'marker_line2'

# bash: output cap keeps the tail - seq output is non-uniform, so the kept
# tail ends with the largest number. A head-keeping cap would drop it.
run_bash_test "bash: output cap keeps tail" 0 '{"command":"seq 1 50000"}' '' '' \
  'truncated:true' 'marker_line2' 'stdout_contains:50000'

# bash: cwd - run in a known directory, pwd returns it
mkdir -p "$TEST_DIR/cwd"
run_bash_test "bash: cwd" 0 '{"command":"pwd"}' '' "$TEST_DIR/cwd" \
  "stdout_equals:$TEST_DIR/cwd"

# bash: pipeline - wc -c counts the echo output
run_bash_test "bash: pipeline" 0 '{"command":"echo hello | wc -c"}' '' '' 'stdout_equals:6'

# bash: spawn failure - PATH=/dev/null hides sh, tool-level failure
run_bash_test "bash: spawn failure" nonzero '{"command":"echo hi"}' 'PATH=/dev/null' '' 'stderr_diag'

# bash: empty command - valid, exit 0, empty output
run_bash_test "bash: empty command" 0 '{"command":""}' '' '' 'ec:0' 'stdout_empty'

# bash: timeout zero - rejected
run_bash_test "bash: timeout zero" nonzero '{"command":"echo hi","timeout_secs":0}' '' '' 'stderr_diag'

# bash: timeout negative - rejected
run_bash_test "bash: timeout negative" nonzero '{"command":"echo hi","timeout_secs":-1}' '' '' 'stderr_diag'

# bash: timeout too large - rejected (above the hard cap)
run_bash_test "bash: timeout too large" nonzero '{"command":"echo hi","timeout_secs":500}' '' '' 'stderr_diag'

# bash: stdin EOF - cat gets EOF on the closed stdin, exit 0, empty output
run_bash_test "bash: stdin EOF" 0 '{"command":"cat"}' '' '' 'ec:0' 'stdout_empty'

echo ""
echo "=== Lean-Verify Tool Tests ==="

# lean-verify is extension-owned (rushi-exts/goal-tools/lean-verify/,
# docs/tui-ext-repo-split.md section 4, item 16): resolve the built
# binary from the sibling exts checkout (bootstrap; override with
# EXTS_ROOT).
EXTS_ROOT="${EXTS_ROOT:-$(cd "$SCRIPT_DIR/../../rushi-exts" 2>/dev/null && pwd)}"
LV_BIN=""
if [ -n "$EXTS_ROOT" ]; then
  for d in "$EXTS_ROOT/goal-tools/lean-verify/target/debug" "$EXTS_ROOT/goal-tools/lean-verify/target/release"; do
    if [ -x "$d/lean-verify" ]; then
      LV_BIN="$d/lean-verify"
      break
    fi
  done
fi
if [ -z "$LV_BIN" ]; then
  echo "SKIP: lean-verify binary not found (extension-owned: cargo build in rushi-exts/goal-tools/lean-verify)"
else
  # P2 self-doc: --help documents the operations (no SKILL.md).
  LV_HELP_OK=1
  "$LV_BIN" --help > "$TEST_DIR/lv_help" 2>&1 || LV_HELP_OK=0
  if [ "$LV_HELP_OK" -eq 1 ] \
     && rg --fixed-strings "Operations" "$TEST_DIR/lv_help" > /dev/null 2>&1 \
     && rg --fixed-strings "translate" "$TEST_DIR/lv_help" > /dev/null 2>&1; then
    echo "PASS: lean-verify: self-doc (--help documents the operations)"
    PASSED=$((PASSED + 1))
  else
    echo "FAIL: lean-verify: self-doc (--help missing or undocumented)"
    FAILED=$((FAILED + 1))
  fi

  # P5 fenced failures: tool-level validation, exit 1, diagnostic on
  # stderr. All of these fail before any Lean/Aeneas environment is
  # touched, so they need no SKIP guard (lake-bound and aeneas-bound
  # live rows live in scripts/lean-verify-e2e.sh, which SKIPs when the
  # devShells are absent).
  run_test "lean-verify: missing op" 1 "missing required field: op" '' '{}' "$LV_BIN"
  run_test "lean-verify: unknown op" 1 "unknown op" '' '{"op":"frob"}' "$LV_BIN"
  run_test "lean-verify: build bad dir" 1 "does not exist" '' '{"op":"build","dir":"/nonexistent-lv-gate-dir"}' "$LV_BIN"
  run_test "lean-verify: translate without Cargo.toml" 1 "no Cargo.toml" '' "{\"op\":\"translate\",\"dir\":\"$TEST_DIR\"}" "$LV_BIN"
  run_test "lean-verify: init missing name" 1 "init requires" '' '{"op":"init"}' "$LV_BIN"
  run_test "lean-verify: init invalid name" 1 "invalid package name" '' '{"op":"init","name":"1bad"}' "$LV_BIN"

  # P1 route discovery: route resolves the lean-verify manifest from
  # the exts extra tools root (RUSHI_EXTRA_TOOLS_ROOT) to the binary
  # (on PATH) and surfaces the tool-level diagnostic.
  ROUTE_BIN="$(cd "$SCRIPT_DIR/../target/debug" 2>/dev/null && pwd)/route"
  if [ -x "$ROUTE_BIN" ] && [ -n "$EXTS_ROOT" ] && [ -d "$EXTS_ROOT/goal-tools" ]; then
    ROUTE_OUT=$(printf '%s' '{"type":"tool_call","id":"lv-1","name":"lean-verify","arguments":{"op":"init","name":"1bad"}}' \
      | env RUSHI_EXTRA_TOOLS_ROOT="$EXTS_ROOT/goal-tools" PATH="$(dirname "$LV_BIN"):$PATH" \
        "$ROUTE_BIN" --tools "$TOOLS_DIR" --cwd "$TEST_DIR" 2>/dev/null || true)
    if printf '%s' "$ROUTE_OUT" | rg --fixed-strings "invalid package name" > /dev/null 2>&1; then
      echo "PASS: lean-verify: route discovery (tool ran via route, diagnostic surfaced)"
      PASSED=$((PASSED + 1))
    else
      echo "FAIL: lean-verify: route discovery - no 'invalid package name' in route output"
      FAILED=$((FAILED + 1))
    fi
  else
    echo "SKIP: lean-verify route discovery (route binary or exts goal-tools unavailable)"
  fi
fi

echo ""
echo "=== Results: $PASSED passed, $FAILED failed ==="

if [ "$FAILED" -gt 0 ]; then
  exit 1
fi
