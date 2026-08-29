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
# - context fullness, the number to watch for compaction: the last
#   measured request input tokens over the model window, in the
#   starship-statusline style (ctx <pct>% (<tokens>/<window>)). The
#   window is the active model's context_tokens from $CONFIG; the
#   metric is the last usage event's input_tokens, not the
#   cumulative totals. The section hides until both are known.
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
cached_total=0
# The last request's measured input tokens. Context fullness rides
# on this value, not on the cumulative totals.
last_in=0

# Context window cache, keyed on the model name. The model name comes
# from the tick payload; the window is the model's context_tokens in
# $CONFIG. A model change re-parses; the same model reuses the value.
ctx_model=""
ctx_window=""

# Git state cache, TTL 3 s. The refresh runs in a background job:
# a tick reply never waits on a slow git (a cold cache can take
# seconds, and the staleness bound is 3 x tick_ms). The job writes
# one "branch dirty" line to a state file; the next tick loads it.
# The row shows the last loaded state until then, git:none at start.
git_branch=""
git_dirty=0
git_ts=$SECONDS
git_job_pid=""
GIT_TTL=3
# The state file lives in a temp file, not the entry dir: the entry
# dir is the user's checkout, and the host chdirs the script there.
GIT_STATE="$(mktemp 2>/dev/null)"
[ -n "$GIT_STATE" ] || GIT_STATE="${EXT_DIR:-.}/.git_state"

git_refresh() {
  local now=$SECONDS
  if (( now - git_ts < GIT_TTL )); then
    return
  fi
  # A running job already owns the refresh; one job at a time.
  [ -n "$git_job_pid" ] && kill -0 "$git_job_pid" 2>/dev/null && return
  git_ts=$now
  # A background job: the tick reply does not wait on git. The job
  # dies with the process group on quit.
  (
    local b d
    b=$(git -C "$DIR" rev-parse --abbrev-ref HEAD 2>/dev/null || true)
    d=$(git -C "$DIR" status --porcelain 2>/dev/null | wc -l | tr -d '[:space:]')
    printf '%s %s\n' "${b:-none}" "${d:-0}" > "$GIT_STATE"
  ) &
  git_job_pid=$!
}

git_load() {
  # Load the newest state file without waiting on its writer.
  local st
  st=$(cat "$GIT_STATE" 2>/dev/null) || return 0
  [ -n "$st" ] || return 0
  git_branch=${st%% *}
  git_dirty=${st##* }
  [ "$git_branch" = "none" ] && git_branch=""
  [ "$git_dirty" = "0" ] && git_dirty=0
}

# The active model's context window from $CONFIG. Prints nothing
# when the model or the value is unknown: the ctx section hides.
# The config shape is the harness config: a [model."<name>"] section
# carrying a plain `context_tokens = N` key.
ctx_window_of() {
  # $1 = the model name
  local model=$1
  [ -n "${CONFIG:-}" ] || return 0
  awk -v model="$model" '
    /^\[model\./ {
      s = $0
      sub(/^\[model\./, "", s)
      sub(/\].*$/, "", s)
      gsub(/"/, "", s)
      insec = (s == model)
      next
    }
    /^\[/ { insec = 0 }
    insec && /^[[:space:]]*context_tokens[[:space:]]*=/ {
      v = $0
      sub(/^[^=]*=/, "", v)
      gsub(/[" \t\r]/, "", v)
      print v
      exit
    }
  ' "$CONFIG" 2>/dev/null
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

# One line: the usage totals from a compact "in out cached" triple.
usage_add() {
  # $1 = "in out cached", or empty when the event carries no usage.
  [ -n "$1" ] || return 0
  local in out cached
  # Function-local positionals: the main loop's read keeps its own.
  set -- $1
  in=${1:-0}
  out=${2:-0}
  cached=${3:-0}
  in_total=$(( in_total + in ))
  out_total=$(( out_total + out ))
  cached_total=$(( cached_total + cached ))
  # The last measured request is the context-fullness source.
  last_in=$in
}

# Extract "in out cached" from one op line. jq when available; a sed
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
      | "\(.input_tokens // 0) \(.output_tokens // 0) \(.cached_tokens // 0)"
    ' 2>/dev/null
    return 0
  fi
  local in out cached
  in=$(printf '%s' "$line" | sed -n \
    's/.*"usage":{[^}]*"input_tokens":\([0-9]*\).*/\1/p')
  out=$(printf '%s' "$line" | sed -n \
    's/.*"usage":{[^}]*"output_tokens":\([0-9]*\).*/\1/p')
  cached=$(printf '%s' "$line" | sed -n \
    's/.*"usage":{[^}]*"cached_tokens":\([0-9]*\).*/\1/p')
  if [ -n "$in" ] && [ -n "$out" ]; then
    printf '%s %s %s\n' "$in" "$out" "${cached:-0}"
  fi
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

# The context-fullness section, starship-statusline style:
# `ctx <pct>% (<tokens>/<window>)`. Both numbers need to be known,
# so the section hides until the last usage event and the config
# window are both in hand. The window is the model's context_tokens;
# the tokens are the last measured request input, the same value the
# auto-compact budget watches.
ctx_text() {
  [ -n "$ctx_window" ] || return 0
  [ "$last_in" -gt 0 ] 2>/dev/null || return 0
  [ "$ctx_window" -gt 0 ] 2>/dev/null || return 0
  local pct10 pct tok win
  pct10=$(( last_in * 1000 / ctx_window ))
  pct="$(( pct10 / 10 )).$(( pct10 % 10 ))"
  tok=$(_fmt_num "$last_in")
  win=$(_fmt_num "$ctx_window")
  printf 'ctx:%s%% (%s/%s)' "$pct" "$tok" "$win"
}

# k/M abbreviation, the starship-statusline fmtNum rule.
_fmt_num() {
  local n=$1
  if [ "$n" -lt 1000 ]; then
    printf '%s' "$n"
  elif [ "$n" -lt 1000000 ]; then
    awk "BEGIN{printf \"%.1fk\", $n/1000}"
  else
    awk "BEGIN{printf \"%.1fM\", $n/1000000}"
  fi
}

# The usage totals from one tick: the cumulative line, plus the
# cached total when the session saw any, plus the ctx section when
# both its inputs are known.
usage_text() {
  local t="in:${in_total} out:${out_total} sum:$((in_total + out_total))"
  if [ "$cached_total" -gt 0 ] 2>/dev/null; then
    t="$t R:$( _fmt_num "$cached_total" )"
  fi
  local c
  c="$(ctx_text)"
  [ -n "$c" ] && t="$t $c"
  printf '%s' "$t"
}

emit_status() {
  # $1 width, $2 session, $3 model, $4 running(0|1), $5 tick line
  local width=$1 sess=$2 model=$3 run=$4 tickline=$5
  local dirty="" st
  [ "$git_dirty" -gt 0 ] 2>/dev/null && dirty="*(${git_dirty})"
  [ "$run" = "1" ] && st="running" || st="idle"
  local stt
  stt=$(status_text "$tickline")

  # The context window is keyed on the model name from the tick.
  # The model rarely changes; the re-parse only fires on a switch.
  if [ "$model" != "$ctx_model" ]; then
    ctx_window="$(ctx_window_of "$model")"
    ctx_model=$model
  fi

  # Truncate the dir to the last 16 chars with a leading ellipsis.
  local dir=$DIR
  if [ ${#dir} -gt 16 ]; then
    dir="...${dir: -16}"
  fi

  local usage
  usage="$(usage_text)"

  if [ "$width" -ge 100 ]; then
    local line
    line=" [$dir] (git:${git_branch:-none}${dirty}) ${sess:-no-session} ${model:-no-model} ${st} ${usage}"
    [ -n "$stt" ] && line="$line st:${stt}"
    line="${line:0:width}"
    printf '{"v":1,"op":"status","lines":[["%s",{"fg":"darkgray"}]]}\n' "$(esc "$line")"
  else
    local l1 l2
    l1=" [$dir] (git:${git_branch:-none}${dirty}) ${sess:-no-session} ${st}"
    l2="${model:-no-model} ${usage}"
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
  git_load
  # Tick parsing is pure bash: the tick payload is small and the row
  # rebuilds once per second. `width` is the terminal width; terminals
  # under 100 cols get the two-line layout.
  local width=80
  local m_width='"width":'
  case "$1" in *"$m_width"*)
    # Take the value only: cut at the next comma or quote, then
    # strip non-digits. A wider strip grabs the digits of the keys
    # that follow (thinking, seq, model) and fakes a 4-col width.
    local w="${1#*"$m_width"}"
    # The value ends at the next comma: the tick payload is one
    # object, and width is never its last key.
    w="${w%%,*}"
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
