#!/usr/bin/env bash
# tests/nix/run.sh — nix gate: meta.rushi eval-time derivation (issue #13)
# and the TUI binary-name cut-over (issue #31).
#
# Usage: bash tests/nix/run.sh
#
# Prerequisites: nix with flakes enabled, jq, rg.
# The kernel must be buildable (cargo deps in the Nix store).
#
# The script runs two phases:
#   1. Eval-only checks (fast, no building)
#   2. Build checks (kernel + configured packages; slow)
#
# Expected failures (case-hook-guard, case-meta-mismatch) are asserted
# on their stderr, not exit code.

set -u
cd "$(git rev-parse --show-toplevel)"
FL="./tests/nix"
PASS=0
FAIL=0

ok()  { printf '  PASS  %s\n' "$1"; PASS=$((PASS+1)); }
bad() { printf '  FAIL  %s\n' "$1"; FAIL=$((FAIL+1)); }

assert_eq() {
  local desc="$1" got="$2" want="$3"
  if [ "$got" = "$want" ]; then
    ok "$desc"
  else
    bad "$desc (got: $got, want: $want)"
  fi
}

contains() {
  local desc="$1" haystack="$2" needle="$3"
  if printf '%s' "$haystack" | rg -q -- "$needle"; then
    ok "$desc"
  else
    bad "$desc (missing: $needle)"
  fi
}

not_contains() {
  local desc="$1" haystack="$2" needle="$3"
  if printf '%s' "$haystack" | rg -qF -- "$needle"; then
    bad "$desc (unexpected: $needle)"
  else
    ok "$desc"
  fi
}

# Fixed-string contains (for needles with regex metacharacters).
contains_f() {
  local desc="$1" haystack="$2" needle="$3"
  if printf '%s' "$haystack" | rg -qF -- "$needle"; then
    ok "$desc"
  else
    bad "$desc (missing: $needle)"
  fi
}

# Append the full build log to a failed build's captured output.
# `nix build` may only print a pointer line ("For full logs, run: nix log ...").
append_drv_log() {
  local log="$1" drv
  # Prefer the explicit "nix log <drv>" pointer that nix prints when a
  # build fails; fall back to the first .drv path mentioned.
  drv=$(printf '%s' "$log" | rg -o '/nix/store/[^ ]+\.drv' | head -1)
  if [ -n "$drv" ]; then
    log="$log
$(nix log "$drv" 2>&1)"
  fi
  printf '%s' "$log"
}

# ── Phase 1: eval-only assertions ────────────────────────────────
echo "── Phase 1: eval-only assertions ──"

# 1a. meta case: extension_tool_paths filled from meta.rushi.entry
val=$(nix eval "$FL#evals.meta.extToolPaths" --json 2>/dev/null)
assert_eq "meta: extToolPaths == [\"tools/goal\"]" "$val" '["tools/goal"]'

# 1b. meta case: ui_extension_names filled from meta.rushi.ext
val=$(nix eval "$FL#evals.meta.uiNames" --json 2>/dev/null)
assert_eq "meta: uiNames == [\"goal\"]" "$val" '["goal"]'

# 1c. meta case: no fallback warnings on clean eval
warn=$(nix eval "$FL#evals.meta.extToolPaths" --json 2>&1 >/dev/null)
not_contains "meta: no fallback warning" "$warn" "falling back to build-time discovery"

# 1d. user-authority: consumer-set extension_tool_paths wins
val=$(nix eval "$FL#evals.userAuth.extToolPaths" --json 2>/dev/null)
assert_eq "userAuth: extToolPaths == [\"tools/custom\"]" "$val" '["tools/custom"]'

# 1e. legacy fallback: eval-time value covers only meta sources
val=$(nix eval "$FL#evals.legacy.extToolPaths" --json 2>/dev/null)
assert_eq "legacy: eval-time extToolPaths == [\"tools/goal\"]" "$val" '["tools/goal"]'

# 1f. legacy fallback: warning emitted for the legacy source
warn=$(nix eval "$FL#evals.legacy.extToolPaths" 2>&1 >/dev/null)
contains "legacy: fallback warning present" "$warn" "falling back to build-time discovery"

# 1g. plain-path: eval-time value is empty, warning emitted
val=$(nix eval "$FL#evals.plainPath.extToolPaths" --json 2>/dev/null)
assert_eq "plainPath: eval-time extToolPaths == []" "$val" '[]'
warn=$(nix eval "$FL#evals.plainPath.extToolPaths" 2>&1 >/dev/null)
contains "plainPath: fallback warning present" "$warn" "falling back to build-time discovery"

# 1h. probe: lib.warn available in pinned nixpkgs
val=$(nix eval "$FL#probe" 2>/dev/null)
assert_eq "probe: lib.warn returns 42" "$val" "42"

# ── Phase 2: build assertions (slow) ─────────────────────────────
echo "── Phase 2: build assertions ──"

# 2a. case-meta: build succeeds, config.toml byte-identical to eval
out=$(nix build "$FL#packages.x86_64-linux.case-meta" --print-out-paths 2>/dev/null)
[ -n "$out" ] && [ -d "$out" ] && ok "case-meta: build succeeded" || bad "case-meta: build failed"
if [ -n "$out" ]; then
  nix eval "$FL#evals.meta.configText" --raw > /tmp/.mk13_eval.toml 2>/dev/null
  if diff -q /tmp/.mk13_eval.toml "$out/config.toml" >/dev/null 2>&1; then
    ok "case-meta: config.toml byte-identical to eval config"
  else
    bad "case-meta: config.toml differs from eval config"
  fi
  cfg=$(cat "$out/config.toml")
  contains "case-meta: config has tools/goal" "$cfg" 'tools/goal'
  man=$(cat "$out/tools.manifest" 2>/dev/null)
  contains "case-meta: manifest has goal ext" "$man" '"goal"'
  [ -f "$out/tools/goal/tool.toml" ] && ok "case-meta: tools/goal exists" || bad "case-meta: tools/goal missing"
  [ -f "$out/ui_extensions/goal/ext.toml" ] && ok "case-meta: ui_extensions/goal exists" || bad "case-meta: ui_extensions/goal missing"
  [ -f "$out/hooks/harness-hook-fake" ] && ok "case-meta: hooks/harness-hook-fake exists" || bad "case-meta: hooks/harness-hook-fake missing"
  rm -f /tmp/.mk13_eval.toml
fi

# 2b. case-legacy: build succeeds, config has both meta + discovered paths
out=$(nix build "$FL#packages.x86_64-linux.case-legacy" --print-out-paths 2>/dev/null)
[ -n "$out" ] && [ -d "$out" ] && ok "case-legacy: build succeeded" || bad "case-legacy: build failed"
if [ -n "$out" ]; then
  cfg=$(cat "$out/config.toml")
  contains "case-legacy: config has tools/goal" "$cfg" 'tools/goal'
  contains "case-legacy: config has tools/legacy-tool" "$cfg" 'tools/legacy-tool'
  man=$(cat "$out/tools.manifest" 2>/dev/null)
  contains "case-legacy: manifest has goal" "$man" '"goal"'
  contains "case-legacy: manifest has legacy-ext" "$man" '"legacy-ext"'
fi

# 2c. case-user-auth: build succeeds, user value is authoritative
out=$(nix build "$FL#packages.x86_64-linux.case-user-auth" --print-out-paths 2>/dev/null)
[ -n "$out" ] && [ -d "$out" ] && ok "case-user-auth: build succeeded" || bad "case-user-auth: build failed"
if [ -n "$out" ]; then
  nix eval "$FL#evals.userAuth.configText" --raw > /tmp/.mk13_eval.toml 2>/dev/null
  if diff -q /tmp/.mk13_eval.toml "$out/config.toml" >/dev/null 2>&1; then
    ok "case-user-auth: config.toml byte-identical to eval config"
  else
    bad "case-user-auth: config.toml differs from eval config"
  fi
  cfg=$(cat "$out/config.toml")
  contains "case-user-auth: config has tools/custom" "$cfg" 'tools/custom'
  not_contains "case-user-auth: config has no tools/goal" "$cfg" 'tools/goal'
  rm -f /tmp/.mk13_eval.toml
fi

# 2d. case-plain-path: build succeeds, discovery fills the path
out=$(nix build "$FL#packages.x86_64-linux.case-plain-path" --print-out-paths 2>/dev/null)
[ -n "$out" ] && [ -d "$out" ] && ok "case-plain-path: build succeeded" || bad "case-plain-path: build failed"
if [ -n "$out" ]; then
  cfg=$(cat "$out/config.toml")
  contains "case-plain-path: config has tools/plain-tool" "$cfg" 'tools/plain-tool'
fi

# 2e. case-hook-guard: build must FAIL with the drift-guard message
guard_log=$(nix build "$FL#packages.x86_64-linux.case-hook-guard" 2>&1)
guard_rc=$?
if [ $guard_rc -ne 0 ]; then
  guard_log=$(append_drv_log "$guard_log")
  contains_f "case-hook-guard: build failed with guard message" "$guard_log" 'hook command(s) not found'
  contains_f "case-hook-guard: names the missing hook" "$guard_log" 'harness-hook-never-shipped'
else
  bad "case-hook-guard: build unexpectedly succeeded"
fi

# 2f. case-meta-mismatch: build must FAIL with the meta-check message
mm_log=$(nix build "$FL#packages.x86_64-linux.case-meta-mismatch" 2>&1)
mm_rc=$?
if [ $mm_rc -ne 0 ]; then
  mm_log=$(append_drv_log "$mm_log")
  contains_f "case-meta-mismatch: build failed with meta-check" "$mm_log" "meta.rushi.entry 'nope' declared but"
else
  bad "case-meta-mismatch: build unexpectedly succeeded"
fi

# 2g. case-tui-new (issue #31): post-rushi-tui#22 TUI pin ships
# bin/rushi-tui only. The configured package must gain an executable
# bin/rushi-tui next to config.toml, with no bin/tui alias and no
# "TUI unavailable" warning.
out=$(nix build "$FL#packages.x86_64-linux.case-tui-new" --print-out-paths 2>/dev/null)
[ -n "$out" ] && [ -d "$out" ] && ok "case-tui-new: build succeeded" || bad "case-tui-new: build failed"
if [ -n "$out" ] && [ -d "$out" ]; then
  [ -f "$out/bin/rushi-tui" ] && ok "case-tui-new: bin/rushi-tui present" || bad "case-tui-new: bin/rushi-tui missing"
  [ -x "$out/bin/rushi-tui" ] && ok "case-tui-new: bin/rushi-tui executable" || bad "case-tui-new: bin/rushi-tui not executable"
  [ ! -e "$out/bin/tui" ] && ok "case-tui-new: no bin/tui alias" || bad "case-tui-new: unexpected bin/tui alias"
  [ -f "$out/config.toml" ] && ok "case-tui-new: config.toml next to bin/" || bad "case-tui-new: config.toml missing"
  log=$(nix log "$out" 2>/dev/null)
  not_contains "case-tui-new: no TUI-unavailable warning" "$log" "TUI unavailable"
fi

# 2h. case-tui-old (issue #31): pre-#22 TUI pin still ships the old
# bin/tui. Clean cut-over: the build succeeds with the old-name
# warning and ships no TUI binary at all (no fallback copy).
out=$(nix build "$FL#packages.x86_64-linux.case-tui-old" --print-out-paths 2>/dev/null)
[ -n "$out" ] && [ -d "$out" ] && ok "case-tui-old: build succeeded" || bad "case-tui-old: build failed"
if [ -n "$out" ] && [ -d "$out" ]; then
  [ ! -e "$out/bin/tui" ] && ok "case-tui-old: no bin/tui fallback" || bad "case-tui-old: unexpected bin/tui"
  [ ! -e "$out/bin/rushi-tui" ] && ok "case-tui-old: no bin/rushi-tui fallback" || bad "case-tui-old: unexpected bin/rushi-tui"
  log=$(nix log "$out" 2>/dev/null)
  contains "case-tui-old: old-name warning emitted" "$log" "has no bin/rushi-tui; TUI unavailable"
fi

# ── Summary ───────────────────────────────────────────────────────
echo ""
echo "Result: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ] || exit 1
