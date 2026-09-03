#!/usr/bin/env bash
# The reference `frame` extension (ui-extension-plan frame stage).
#
# One long-lived process. It answers every `frame` op (and the tick
# cadence ping) with a `frame_spec` that colors the input area's
# rounded border to correlate with the active model's thinking level,
# and labels the frame with the current editor mode:
#
# - thinking 0 (no thinking published): idle gray (the pi
#   `thinkingOff` color, catppuccin-macchiato `overlay1`)
# - thinking 1 (low): teal (the pi `thinkingLow` color, `teal`)
# - thinking 2 (medium): green (the pi `thinkingMedium` color, `green`)
# - thinking 3 (high): yellow (the pi `thinkingHigh` color, `yellow`)
# - thinking 4+ (highest): peach (the pi `thinkingXhigh` color,
#   `peach`)
#
# The hex values are the pi `catppuccin-macchiato` theme
# `thinking*` border colors (docs/tui-color-pi-alignment.md), the
# same table as the host's `catppuccin macchiato` scheme
# `border0`..`border4` roles and the reference statusline palette
# (docs/tui-statusline-powerline.md). The host lowers them to the
# active terminal capability level at storage (ext.rs
# `lower_frame_spec`). A level the table does not know keeps the
# idle gray.
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

# The thinking-level border colors: the pi catppuccin-macchiato
# theme `thinking*` values (the host `catppuccin macchiato` scheme
# `border0`..`border4` table, docs/tui-color-pi-alignment.md).
# `frame.sh` is a reference extension: it carries the hex values of
# that table, which the host lowers to the terminal level.
frame_color() {
  case "$1" in
    0) printf '#8087a2' ;;
    1) printf '#8bd5ca' ;;
    2) printf '#a6da95' ;;
    3) printf '#eed49f' ;;
    *) printf '#f5a97f' ;;
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
