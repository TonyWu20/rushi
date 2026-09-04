#!/usr/bin/env bash
# verify-specs.sh — Lean-style doc gate.
#
# Checks that every in-scope spec doc in docs/ carries the three
# required sections (Properties, Verification, Gate), that every
# property has a Verification table row, and that no doc carries a
# bare `sorry`/`admit` token. This is the docs half of the
# Lean-driven gate; the code half is `cargo build` + `cargo test`
# plus the e2e scripts named in each Gate section.
#
# Exit 0 = clean, 1 = one or more docs fail.

set -uo pipefail

DOCS_DIR="$(cd "$(dirname "$0")/../docs" && pwd)"
FAIL=0

# In-scope: spec / design docs (the ones that define behavior).
# Excluded: reviews, audits, records, investigations, process docs,
# INDEX.md.
IN_SCOPE=(
  "architecture.md"
  "auto-compact-plan.md"
  "bash-tool.md"
  "ft-005-logline.md"
  "handoff-strategy.md"
  "loop-and-edit-implementation.md"
  "loop-and-edit-tool.md"
  "loop-lifecycle-hooks.md"
  "phase-2-plan.md"
  "skill-remapped-to-os-apps.md"
  "tui-color-pi-alignment.md"
  "tui-color-scheme.md"
  "tui-color-tones.md"
  "tui-command-palette.md"
  "tui-conversation-browsing.md"
  "tui-file-picker.md"
  "tui-markdown-render.md"
  "tui-model-wait-indicator.md"
  "tui-pending-user-messages.md"
  "tui-statusline-powerline.md"
  "tui-streaming-response.md"
  "tui-syntax-highlighting.md"
  "tui-thinking-block.md"
  "tui-thinking-level-input-box.md"
  "tui-tool-display-port.md"
  "tui-tool-result-truncation.md"
  "tui.md"
  "ui-extension-plan.md"
  "ui-extension.md"
  "user-message-editing.md"
  "vim-editor-design.md"
)

check_doc() {
  local doc="$1"
  local path="$DOCS_DIR/$doc"
  local doc_fail=0

  if [ ! -f "$path" ]; then
    echo "  MISSING: $doc"
    FAIL=1
    return
  fi

  # 0. Exemption: a pure design-discussion doc states why it has no
  # behavioral properties. It skips the section checks.
  if rg -q "No behavioral properties" "$path" 2>/dev/null; then
    echo "  EXEMPT: $doc (no behavioral properties)"
    return
  fi

  # 1. Required sections.
  for section in "Properties" "Verification" "Gate"; do
    if ! rg -q "^## ${section}\b" "$path" 2>/dev/null; then
      echo "  FAIL: $doc missing '## ${section}'"
      doc_fail=1
    fi
  done

  # 2. Every P<n> property has a Verification table row. The first
  # table cell may be `P<n>` or `<n>`.
  if rg -q "^## Properties\b" "$path" 2>/dev/null; then
    local props
    props=$(rg -o '^P[0-9]+' "$path" 2>/dev/null | sed 's/^P//' | sort -u)
    if [ -z "$props" ]; then
      echo "  FAIL: $doc '## Properties' has no P<n> lines"
      doc_fail=1
    fi
    for num in $props; do
      if ! rg -q "^\\| *(P)?${num} *\\|" "$path" 2>/dev/null; then
        echo "  FAIL: $doc property P${num} has no Verification table row"
        doc_fail=1
      fi
    done
  fi

  # 3. No bare sorry / admit tokens. Lines that state the rule itself
  # are exempt.
  local hits
  hits=$(rg -n '\bsorry\b|\badmit\b' "$path" 2>/dev/null |
    grep -vE 'no sorry|no admit|No sorry|No admit|sorry/admit|"sorry"|"admit"|sorry`|admit`' || true)
  if [ -n "$hits" ]; then
    echo "  FAIL: $doc carries a bare sorry/admit token:"
    echo "$hits" | sed 's/^/      /'
    doc_fail=1
  fi

  if [ "$doc_fail" -eq 0 ]; then
    echo "  PASS: $doc"
  else
    FAIL=1
  fi
}

echo "verify-specs: checking ${#IN_SCOPE[@]} docs"
echo

for doc in "${IN_SCOPE[@]}"; do
  check_doc "$doc"
done

echo
if [ "$FAIL" -eq 0 ]; then
  echo "verify-specs: clean"
else
  echo "verify-specs: FAIL"
  exit 1
fi
