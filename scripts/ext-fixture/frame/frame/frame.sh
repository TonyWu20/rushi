#!/usr/bin/env bash
# A frame extension fixture: label the input frame with the editor
# mode of each `frame` op (the reference frame.sh behavior, minus
# the thinking-level colors).
set -u

esc() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  printf '%s' "$s"
}

# The mode label of one op line: the string after "mode":" up to the
# next quote. A missing value is the idle label. The `fx` prefix
# marks the fixture's own label so the smoke test can tell it apart
# from the host built-in label (the same mode text without the
# prefix).
mode_of() {
  local line=$1
  local mode="IDLE"
  local m='"mode":"'
  case "$line" in
    *"$m"*)
      local v="${line#*"$m"}"
      mode="${v%%\"*}"
      [ -n "$mode" ] || mode="IDLE"
      ;;
  esac
  printf 'fx-%s' "$mode"
}

emit_frame_spec() {
  # $1 = the mode label
  local label
  label=$(esc "$1")
  printf '{"v":1,"op":"frame_spec","spec":{"label":{"lines":["%s"],"style":{"fg":"darkgray","bold":true}}}}\n' \
    "$label"
}

while IFS= read -r line; do
  case "$line" in
    *'"op":"frame"'*)
      emit_frame_spec "$(mode_of "$line")"
      ;;
  esac
done
