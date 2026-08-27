#!/usr/bin/env bash
# The reference notify extension (ui-extension-plan stage 2).
#
# Watches assistant_message events (the manifest's kinds filter). A
# finished turn is stop_reason `stop` or `length`. The script rings
# through the host's `notify` op:
#   - kind `bell`: the host writes the terminal bell
#   - kind `osc`: the host writes OSC 0 (window title) with the
#     finished-turn text
#
# Burst suppression: at start the host re-sends the whole visible
# transcript. Ringing once per finished turn would be a burst. For
# the first 5 s of process life the script only remembers the most
# recent finished turn. When the op stream goes quiet past that
# window (or a new event arrives), live mode starts and the
# remembered turn rings once. In live mode every finished turn
# rings; the id guard rings nothing twice.
#
# Tmux tty resolution: under tmux the inner pane's terminal stream
# may not reach the user's terminal. The script resolves the tmux
# client tty and writes the ring there as a best-effort fallback
# channel. The host op stays the primary path.

set -u
# Ring only after the start burst: SECONDS is a bash builtin counter.
LIVE_AFTER_S=5
in_live=0
last_fid=-1
last_ftitle=""

# JSON-string escape without a process spawn.
esc() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  printf '%s' "$s"
}

ring() {
  # $1 = the OSC title text
  local title=$1
  printf '{"v":1,"op":"notify","kind":"bell"}\n'
  printf '{"v":1,"op":"notify","kind":"osc","code":0,"args":"%s"}\n' "$(esc "$title")"
  if [ -n "${TMUX:-}" ]; then
    local tty
    tty=$(tmux display-message -p '#{client_tty}' 2>/dev/null || true)
    if [ -n "$tty" ]; then
      printf '\a' > "$tty" 2>/dev/null || true
      printf '\033]0;%s\007' "$title" > "$tty" 2>/dev/null || true
    fi
  fi
}

on_event() {
  # $1 = one op line
  local line=$1
  # Live mode starts when the op stream is past the start-burst
  # window.
  if [ "$in_live" = 0 ] && [ "$SECONDS" -ge "$LIVE_AFTER_S" ]; then
    in_live=1
  fi
  # Only finished turns ring: stop_reason stop or length.
  case "$line" in
    *'"stop_reason":"stop"'*|*'"stop_reason":"length"'*)
      ;;
    *)
      return 0
      ;;
  esac
  # The op's `id` is the log index. The host serializes op objects
  # with sorted keys, so the op-level `id` sorts after `event` and is
  # the last `"id":` in the line. Marker variable: quoted literals
  # inside ${...} patterns do not survive bash quoting.
  local m_id='"id":'
  local rest="${line##*"$m_id"}"
  local id="${rest%%[!0-9]*}"
  [ -n "$id" ] || return 0
  [ "$id" -gt "$last_fid" ] || return 0
  last_fid=$id
  local title="turn finished"
  local m_content='"content":"'
  case "$line" in *"$m_content"*)
    local c="${line#*"$m_content"}"
    c="${c%%\"*}"
    c="${c:0:40}"
    [ -n "$c" ] && title="turn finished: $c"
    ;;
  esac
  last_ftitle="$title"
  [ "$in_live" = 1 ] && ring "$title"
}

# The op stream went quiet for a read timeout. A quiet stream past
# the burst window means the start resend is over: flush the
# remembered finished turn, if one exists.
on_quiet() {
  [ "$in_live" = 0 ] || return 0
  [ "$SECONDS" -ge "$LIVE_AFTER_S" ] || return 0
  [ "$last_fid" -gt -1 ] || return 0
  in_live=1
  ring "$last_ftitle"
}

while :; do
  if IFS= read -r -t 1 line; then
    case "$line" in
      *'"op":"event"'*)
        on_event "$line"
        ;;
    esac
  else
    rc=$?
    [ "$rc" -le 128 ] && break
    on_quiet
  fi
done
