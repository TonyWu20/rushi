#!/usr/bin/env bash
# The reference statusline extension (ui-extension-plan stage 2).
#
# One long-lived process. It answers every `tick` op with a status
# row. The row shows:
# - live dir: the config dir, where the TUI and the loop operate
#   ($CONFIG is exported by the host)
# - git branch + dirty mark, TTL-cached at 3 s so a tick never
#   spawns git more than once per 3 s (the design's tick-cost note)
# - session, model, and loop state from the tick payload
# - cumulative usage summed over assistant_message.usage events;
#   the host re-sends every usage-bearing message at start, so the
#   totals survive a TUI restart from the log alone
# - ext_status values that other extensions published into the log;
#   the row consumes them through the tick payload's `statuses` map
#
# Layout: one line on wide terminals, two lines when the terminal is
# narrow (width under 100). The host reserves one terminal row per
# line.

set -u
DIR="$(dirname "${CONFIG:-.}")"
USE_JQ=0
command -v jq >/dev/null 2>&1 && USE_JQ=1

in_total=0
out_total=0

# Git state cache, TTL 3 s. SECONDS is a bash builtin counter, so the
# TTL check spawns nothing. The cache starts "fresh": the first tick
# skips the git spawn and the row shows git:none; the refresh lands
# by the fourth tick. A cold git can take seconds on a cold cache,
# and the first tick reply must stay fast (the host gives a
# generation 10 s to its first reply, but the row still waits on it).
git_branch=""
git_dirty=0
git_ts=$SECONDS
GIT_TTL=3

git_refresh() {
  local now=$SECONDS
  if (( now - git_ts < GIT_TTL )); then
    return
  fi
  git_ts=$now
  local b d
  b=$(git -C "$DIR" rev-parse --abbrev-ref HEAD 2>/dev/null || true)
  d=$(git -C "$DIR" status --porcelain 2>/dev/null | wc -l | tr -d '[:space:]')
  [ -n "$b" ] && git_branch="$b"
  [ -n "$d" ] && git_dirty="$d"
}

# JSON-string escape without spawning a process. Covers the values
# this row emits: backslashes, quotes, and control characters.
esc() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\t'/\\t}
  s=${s//$'\n'/\\n}
  printf '%s' "$s"
}

# One line: the usage totals from a compact "in out" pair.
usage_add() {
  # $1 = "in out", or empty when the event carries no usage.
  [ -n "$1" ] || return 0
  in_total=$(( in_total + ${1%% *} ))
  out_total=$(( out_total + ${1#* } ))
}

# Extract "in out" from one op line. jq when available; a sed
# fallback for machines without it. The fallback anchors on the
# flat `"usage":{...}` object, so tool_calls with nested objects
# earlier in the line cannot fragment the match.
usage_pair() {
  local line=$1
  if [ "$USE_JQ" = 1 ]; then
    printf '%s' "$line" | jq -r '
      select((.op // "") == "event")
      | select((.event.type // "") == "assistant_message")
      | .event.usage // empty
      | "\(.input_tokens // 0) \(.output_tokens // 0)"
    ' 2>/dev/null
    return 0
  fi
  local in out
  in=$(printf '%s' "$line" | sed -n \
    's/.*"usage":{[^}]*"input_tokens":\([0-9]*\).*/\1/p')
  out=$(printf '%s' "$line" | sed -n \
    's/.*"usage":{[^}]*"output_tokens":\([0-9]*\).*/\1/p')
  [ -n "$in" ] && [ -n "$out" ] && printf '%s %s\n' "$in" "$out"
}

# The ext_status values of the tick, compact "k=v" text. The row
# shows at most two; the rest stay in the log. An empty statuses map
# skips the jq spawn.
status_text() {
  # $1 = the tick line
  case "$1" in
    *'"statuses":{}'*) return 0 ;;
  esac
  local out=""
  if [ "$USE_JQ" = 1 ]; then
    out=$(printf '%s' "$1" | jq -r '
      ((.statuses // {}) | to_entries)
      | .[0:2][]
      | "\(.key)=\(.value | if type == "string" then . else tojson end)"
    ' 2>/dev/null)
  fi
  printf '%s' "${out:-}"
}

emit_status() {
  # $1 width, $2 session, $3 model, $4 running(0|1), $5 tick line
  local width=$1 sess=$2 model=$3 run=$4 tickline=$5
  local dirty="" st
  [ "$git_dirty" -gt 0 ] 2>/dev/null && dirty="*(${git_dirty})"
  [ "$run" = "1" ] && st="running" || st="idle"
  local stt
  stt=$(status_text "$tickline")

  # Truncate the dir to the last 16 chars with a leading ellipsis.
  local dir=$DIR
  if [ ${#dir} -gt 16 ]; then
    dir="...${dir: -16}"
  fi

  if [ "$width" -ge 100 ]; then
    local line
    line=" [$dir] (git:${git_branch:-none}${dirty}) ${sess:-no-session} ${model:-no-model} ${st} in:${in_total} out:${out_total} sum:$((in_total + out_total))"
    [ -n "$stt" ] && line="$line st:${stt}"
    line="${line:0:width}"
    printf '{"v":1,"op":"status","lines":[["%s",{"fg":"darkgray"}]]}\n' "$(esc "$line")"
  else
    local l1 l2
    l1=" [$dir] (git:${git_branch:-none}${dirty}) ${sess:-no-session} ${st}"
    l2="${model:-no-model} in:${in_total} out:${out_total} sum:$((in_total + out_total))"
    [ -n "$stt" ] && l2="$l2 st:${stt}"
    l1="${l1:0:width}"
    l2="${l2:0:width}"
    printf '{"v":1,"op":"status","lines":[["%s",{"fg":"darkgray"}],["%s",{"fg":"darkgray"}]]}\n' \
      "$(esc "$l1")" "$(esc "$l2")"
  fi
}

on_tick() {
  # $1 = the tick line
  git_refresh
  # Tick parsing is pure bash: the tick payload is small and the row
  # rebuilds once per second. `width` is the terminal width; terminals
  # under 100 cols get the two-line layout.
  local width=80
  local m_width='"width":'
  case "$1" in *"$m_width"*)
    local w="${1#*"$m_width"}"
    width="${w//[!0-9]/}"
    [ -n "$width" ] || width=80
    ;;
  esac
  local sess=""
  local m_sess='"session":"'
  case "$1" in *"$m_sess"*)
    sess="${1#*"$m_sess"}"
    sess="${sess%%\"*}"
    ;;
  esac
  local model=""
  local m_model='"model":"'
  case "$1" in *"$m_model"*)
    model="${1#*"$m_model"}"
    model="${model%%\"*}"
    ;;
  esac
  local m_run='"loop_running":true'
  if [[ "$1" == *"$m_run"* ]]; then
    emit_status "$width" "$sess" "$model" 1 "$1"
  else
    emit_status "$width" "$sess" "$model" 0 "$1"
  fi
}

while IFS= read -r line; do
  case "$line" in
    *'"op":"tick"'*)
      on_tick "$line"
      ;;
    *'"op":"event"'*)
      usage_add "$(usage_pair "$line")"
      ;;
  esac
done
