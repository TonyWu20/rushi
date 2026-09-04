#!/usr/bin/env bash
# Command-palette demo extension (docs/tui-command-palette.md section 10).
#
# Declares the `commands` cap so the host queries it for a command
# list when the `:` palette opens. The script registers two plain
# commands and one setting (with options). The `invoke` op echoes
# the chosen value back in a human-readable reply.
#
# Protocol (docs/tui-command-palette.md section 10):
#   host -> extension: {"v":1,"op":"commands","session":...,"loop_running":...}
#   extension -> host: {"v":1,"op":"commands_list","commands":[...]}
#   host -> extension: {"v":1,"op":"invoke","req":N,"id":"...","value":"..."}
#   extension -> host: {"v":1,"op":"invoke_reply","req":N,"ok":true,"message":"..."}

set -u

# ---------------------------------------------------------------------------
# Read ops from stdin, one JSON line at a time.
# ---------------------------------------------------------------------------
while IFS= read -r line; do
  # Only process JSON lines.
  case "$line" in
    \{*")
      ;;
    *)
      continue
      ;;
  esac

  # --- commands op --------------------------------------------------------
  # Host asks for the command list. Respond with commands_list.
  case "$line" in
    *'"op":"commands"'*)
      printf '%s\n' '{"v":1,"op":"commands_list","commands":[
        {"id":"cmd_palette.reload","label":"Reload demo config","kind":"run","hint":"Ctrl+R","help":"Re-reads the demo config file and reloads the settings panel."},
        {"id":"cmd_palette.ping","label":"Ping","kind":"run","hint":"","help":"Sends a ping to the demo extension process and returns the round-trip time."},
        {"id":"cmd_palette.theme","label":"Theme","kind":"set","hint":"","help":"Set the demo theme.","options":["light","dark","auto"]}
      ]}'
      continue
      ;;
  esac

  # --- invoke op ----------------------------------------------------------
  # Host invokes a command. Respond with invoke_reply.
  case "$line" in
    *'"op":"invoke"'*)
      # Extract the request id and command id with a simple sed
      # (no jq dependency for this demo).
      req=$(printf '%s' "$line" | sed -n 's/.*"req":\([0-9]*\).*/\1/p' | head -n 1)
      id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | head -n 1)
      val=$(printf '%s' "$line" | sed -n 's/.*"value":"\([^"]*\)".*/\1/p' | head -n 1)

      case "$id" in
        cmd_palette.reload)
          msg="config reloaded"
          ;;
        cmd_palette.ping)
          msg="pong (0.3 ms)"
          ;;
        cmd_palette.theme)
          if [ -n "$val" ]; then
            msg="theme set to ${val}"
          else
            msg="theme not set (no value provided)"
          fi
          ;;
        *)
          msg="unknown command: ${id}"
          ;;
      esac

      printf '{"v":1,"op":"invoke_reply","req":%s,"ok":true,"message":"%s"}\n' \
        "${req:-0}" "$msg"
      continue
      ;;
  esac

  # Ignore all other ops (event, tick, transform, lines, status, etc.)
done
