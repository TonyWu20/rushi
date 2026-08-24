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
echo "=== Results: $PASSED passed, $FAILED failed ==="

if [ "$FAILED" -gt 0 ]; then
  exit 1
fi
