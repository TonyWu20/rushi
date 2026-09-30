# rushi-sessions channel as a Nix value (TOML-serializable attrset).
# Source of truth: /home/tony/programming/tv-rushi/rushi-sessions.toml
#
# The source and preview commands are provided by the `rushi-sessions`
# binary (kernel bin/rushi-sessions, shipped in the rushi Nix package).
# It walks for rushi session directories and renders the preview card,
# replacing the previous inline python3 scripts and the `fd`/`python3`
# run-time dependencies.
#
# The display/output/preview templates split fields on a real tab. Nix
# single-quote strings interpret \t as a real tab, so the templates use
# it directly. Multi-line ''-strings keep backslash sequences literal,
# so their commands use @TAB@ markers that `untab` turns into real
# tabs before the channel is serialized.

let
  tab = "\t";
  untab = s: builtins.replaceStrings [ "@TAB@" ] [ tab ] s;

  sourceCommand = "rushi-sessions source";

  previewCommand = untab ''
rushi-sessions preview '{split:@TAB@:0}' '{split:@TAB@:1}' '{split:@TAB@:2}' '{split:@TAB@:3}' '{split:@TAB@:4}' '{split:@TAB@:5}' '{split:@TAB@:6}'
'';
in
{
  metadata = {
    name = "rushi-sessions";
    description = "Peek at rushi sessions: status, loop phase, last activity";
    # Hard requirement: the rushi-sessions binary (ships in the rushi
    # package). `bat` is optional; the preview falls back to plain TOML
    # when it is not on PATH.
    requirements = [ "rushi-sessions" ];
  };

  source = {
    shell = "bash";
    command = sourceCommand;
    display = "[{split:\t:0}] {split:\t:2}/{split:\t:1} [{split:\t:3}] {split:\t:4}";
    output = "{split:\t:6}";
    frecency = false;
  };

  preview = {
    shell = "bash";
    cached = false;
    command = previewCommand;
  };

  ui.preview_panel.size = 50;

  keybindings = {
    "ctrl-e" = "actions:open";
    "ctrl-t" = "actions:tail";
    "ctrl-k" = "actions:kill";
  };

  actions.open = {
    description = "Open this session in rushi-tui (full interaction, forked)";
    shell = "bash";
    mode = "fork";
    command = untab ''
sh -c 's="$1"; n=$(basename "$s"); cdw=$(cat "$s/cwd" 2>/dev/null); if [ -z "$cdw" ] || [ ! -d "$cdw" ]; then cdw=$(dirname "$(dirname "$s")"); fi; cd "$cdw" && rushi-tui "$n"' sh '{split:@TAB@:6}'
'';
  };

  actions.tail = {
    description = "Follow the session event log (Ctrl-C returns to tv)";
    shell = "bash";
    mode = "fork";
    command = "tail -f '{split:\t:6}/events.jsonl'";
  };

  actions.kill = {
    description = "Stop the session loop (SIGTERM to the loop pid)";
    shell = "bash";
    mode = "fork";
    command = untab ''
sh -c 'p=$(cat "$1/loop.pid" 2>/dev/null); if [ -n "$p" ] && kill -0 "$p" 2>/dev/null; then kill -TERM "$p"; echo "sent SIGTERM to $p ("$(basename "$1")")"; else echo "no live loop for "$(basename "$1")" (stale or absent pid)"; fi' sh '{split:@TAB@:6}'
'';
  };
}
