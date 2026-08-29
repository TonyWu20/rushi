#!/usr/bin/env bash
# The reference `frame` extension (ui-extension-plan frame stage).
#
# One long-lived process. It answers every `frame` op (and the tick
# cadence ping) with a `frame_spec` that colors the input area's
# rounded border to correlate with the active model's thinking level,
# and labels the frame with the current editor mode:
#
# - thinking 0 (no thinking published): idle gray
# - thinking 1: blue
# - thinking 2: cyan
# - thinking 3: green
# - thinking 4+: amber
#
# The label is the mode the host passes in the `frame` op (`mode` is
# the editor's modal state, e.g. "INSERT", "NORMAL", "[d-PENDING]").
# The host renders the label on the border; it never types into the
# draft (the frame owns the chrome, not the input state —
# docs/ui-extension.md section 10).
#
# A bad frame_spec is dropped by the host (G5: the last valid frame
# survives); the built-in frame shows when the extension is missing
# or dead.

set -u

# The thinking-level border colors, mirroring the host's built-in
# palette (bin/tui/src/render.rs `thinking_border`). A level the
# table does not know keeps the idle gray.
frame_color() {
  case "$1" in
    0) printf 'darkgray' ;;
    1) printf 'blue' ;;
    2) printf 'cyan' ;;
    3) printf 'green' ;;
    *) printf 'yellow' ;;
  esac
}

# The JSON string escape of a short label: the mode label is host
# text, never user data, so backslash and double-quote are the only
# escapes needed.
esc() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  printf '%s' "$s"
}

# The thinking level of one op line: a bare int after "thinking":,
# stopped at the first comma so a later number in the payload (the
# width) cannot bleed into it. A missing value is 0.
thinking_of() {
  local line=$1
  local thinking=0
  local m='"thinking":'
  case "$line" in
    *"$m"*)
      local v="${line#*"$m"}"
      v="${v%%,*}"
      thinking="${v//[!0-9]/}"
      [ -n "$thinking" ] || thinking=0
      ;;
  esac
  printf '%s' "$thinking"
}

# The mode label of one op line: the string after "mode":" up to the
# next quote. A missing value is the idle label.
mode_of() {
  local line=$1
  local mode="IDLE"
  local m='"mode":"'
  case "$line" in
    *"$m"*)
      local v="${line#*"$m"}"
      mode="${v%%\"*}"
      [ -n "$mode" ] || mode=IDLE
      ;;
  esac
  printf '%s' "$mode"
}

emit_frame_spec() {
  # $1 = the thinking level, $2 = the mode label
  local color
  color=$(frame_color "$1")
  local label
  label=$(esc "$2")
  # The interior height stays the host default (two editor lines):
  # only the border color and the label are owned by this extension.
  printf '{"v":1,"op":"frame_spec","spec":{"border":"rounded","label":{"lines":["%s"],"style":{"fg":"%s","bold":true}}}}\n' \
    "$label" "$color"
}

# One `frame` op.
on_frame() {
  local line=$1
  emit_frame_spec "$(thinking_of "$line")" "$(mode_of "$line")"
}

# One `tick` op (the host also pings the frame owner on its tick
# cadence): keep the frame fresh from the tick's thinking value.
on_tick() {
  local line=$1
  emit_frame_spec "$(thinking_of "$line")" "IDLE"
}

while IFS= read -r line; do
  case "$line" in
    *'"op":"frame"'*)
      on_frame "$line"
      ;;
    *'"op":"tick"'*)
      on_tick "$line"
      ;;
  esac
done
