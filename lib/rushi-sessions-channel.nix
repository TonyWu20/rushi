# rushi-sessions channel as a Nix value (TOML-serializable attrset).
# Source of truth: /home/tony/programming/tv-rushi/rushi-sessions.toml
#
# The original TOML uses basic (triple-quoted) strings, where \t is a
# real tab at parse time. The string-pipeline template engine then
# splits on that real tab. So the Nix strings below must carry real
# tabs, not literal backslash-t. We write @TAB@ markers inside the
# ''-strings and substitute a real tab at the end (a ''-string leaves
# a backslash-t literal, which would break the {split:..} templates).
#
# Nix ''-strings leave backslashes literal, so the embedded Python
# (chr(10), f.seek(...), etc.) transcribes verbatim. The only place
# a backslash must become a tab is the @TAB@ marker.

let
  tab = "\t";
  untab = s: builtins.replaceStrings [ "@TAB@" ] [ tab ] s;

  sourceCommand = untab ''
python3 - <<'PY'
import json, os, subprocess, sys, time
root = os.getcwd()
prune = ["target", ".git", "node_modules", "scratch", ".nix"]
args = ["fd", "--no-ignore", "-t", "d"]
for p in prune:
    args.append("-E")
    args.append(p)
args.append("sessions")
args.append(root)
roots = []
try:
    r = subprocess.run(args, capture_output=True, text=True, timeout=300)
    roots = [l for l in r.stdout.splitlines() if l.strip()]
except Exception:
    roots = []
def alive(pid_s):
    if not pid_s:
        return False
    try:
        os.kill(int(pid_s), 0)
        return True
    except Exception:
        return False
def last_phase(path):
    try:
        size = os.path.getsize(path)
        with open(path, "rb") as f:
            f.seek(max(0, size - 100000))
            data = f.read().decode("utf-8", "replace")
    except Exception:
        return "?"
    i = data.rfind('"id":"loop_phase"')
    if i < 0:
        return "?"
    j = data.find('"value":"', i)
    if j < 0:
        return "?"
    k = data.find('"', j + 9)
    if k < 0:
        return "?"
    return data[j + 9 : k]
rows = []
for sdir in roots:
    base = sdir.rstrip("/")
    try:
        names = os.listdir(base)
    except Exception:
        continue
    repo = os.path.basename(os.path.dirname(base))
    for name in names:
        p = base + "/" + name
        if not os.path.isdir(p):
            continue
        ev = p + "/events.jsonl"
        pidf = p + "/loop.pid"
        cwf = p + "/cwd"
        if not (os.path.exists(ev) or os.path.exists(pidf) or os.path.exists(cwf)):
            continue
        pid = ""
        try:
            pid = open(pidf).read().strip()
        except Exception:
            pass
        status = "ACTIVE" if alive(pid) else "IDLE"
        phase = last_phase(ev) if os.path.exists(ev) else "?"
        ref = ev
        if not os.path.exists(ref):
            ref = pidf if os.path.exists(pidf) else cwf
        m = os.stat(ref).st_mtime
        la = time.strftime("%m-%d %H:%M", time.localtime(m))
        rows.append((status, name, repo, phase, la, str(int(m)), p))
rows.sort(key=lambda r: (r[0] != "ACTIVE", -int(r[5])))
for r in rows:
    print("@TAB@".join(r))
PY
'';

  previewCommand = untab ''
python3 - '{split:@TAB@:0}' '{split:@TAB@:1}' '{split:@TAB@:2}' '{split:@TAB@:3}' '{split:@TAB@:4}' '{split:@TAB@:5}' '{split:@TAB@:6}' <<'PY'
import json, os, sys, subprocess, shutil
status = sys.argv[1]
name = sys.argv[2]
repo = sys.argv[3]
phase = sys.argv[4]
last = sys.argv[5]
path = sys.argv[7]
pid = ""
try:
    pid = open(os.path.join(path, "loop.pid")).read().strip()
except Exception:
    pass
alive = False
if pid:
    try:
        os.kill(int(pid), 0)
        alive = True
    except Exception:
        pass
if pid:
    status = "ACTIVE" if alive else "IDLE"
epath = os.path.join(path, "events.jsonl")
think = ""
last_user = ""
last_assistant = ""
rows = []
if os.path.exists(epath):
    with open(epath, errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                o = json.loads(line)
            except Exception:
                continue
            t = o.get("type")
            if t == "ext_status" and o.get("id") == "model_thinking":
                think = str(o.get("value", ""))
            if t == "user_message":
                last_user = o.get("content") or ""
            if t == "assistant_message":
                last_assistant = o.get("content") or ""
            rows.append(o)
KEEP = ("loop_phase", "model_thinking", "goal_paused", "goal_active", "run.refire")
def brief(o):
    t = o.get("type", "?")
    if t == "tool_call":
        a = o.get("arguments")
        if a is None:
            a = dict()
        s = a.get("command") if isinstance(a, dict) else ""
        if not s:
            s = json.dumps(a)
        return ((o.get("name") or "?") + " " + s).replace(chr(10), " ")
    if t == "tool_result":
        b = o.get("bytes")
        s = "error" if o.get("is_error") else "ok"
        if b is not None:
            s = s + " " + str(b) + "b"
        return s
    if t == "ext_status":
        i = str(o.get("id", "status"))
        v = o.get("value")
        if isinstance(v, (dict, list)):
            return None
        if i not in KEEP and not i.endswith(".error") and i != "hook.exhausted.handle":
            return None
        if v is None:
            return i
        if isinstance(v, str):
            return i + "=" + v
        return i + "=" + json.dumps(v)
    c = o.get("content") or ""
    return c.replace(chr(10), " ")
def clip(s, n):
    s = (s or "").replace(chr(10), " ").replace(chr(13), " ")
    s = " ".join(s.split())
    if len(s) > n:
        s = s[:n] + " ..."
    return s
def q(v):
    return json.dumps(v, ensure_ascii=False)
out = []
out.append("[rushi-session]")
out.append("name    = " + q(name))
out.append("repo    = " + q(repo))
out.append("status  = " + q(status))
out.append("pid     = " + q(pid))
out.append("phase   = " + q(phase))
if think:
    out.append("think   = " + q(think))
out.append("updated = " + q(last))
out.append("")
out.append("[last-user-message]")
out.append("content = " + q(clip(last_user, 400)))
out.append("")
out.append("[last-assistant-message]")
out.append("content = " + q(clip(last_assistant, 400)))
events = []
for o in rows[-40:]:
    b = brief(o)
    if b is None:
        continue
    ts = str(o.get("ts") or "")[11:19]
    events.append((ts, o.get("type", "?"), b))
for ts, ty, b in events[-5:]:
    out.append("")
    out.append("[[recent-event]]")
    out.append("ts    = " + q(ts))
    out.append("type  = " + q(ty))
    out.append("brief = " + q(clip(b, 80)))
card = chr(10).join(out) + chr(10)
theme = os.environ.get("RUSHI_PREVIEW_THEME", "Catppuccin Macchiato")
bat = shutil.which("bat")
if bat:
    try:
        p = subprocess.run([bat, "-l", "toml", "--theme", theme, "--color", "always", "--style", "plain"], input=card, text=True, capture_output=True, timeout=15)
        if p.returncode == 0 and p.stdout:
            sys.stdout.write(p.stdout)
        else:
            sys.stdout.write(card)
    except Exception:
        sys.stdout.write(card)
else:
    sys.stdout.write(card)
PY
'';
in
{
  metadata = {
    name = "rushi-sessions";
    description = "Peek at rushi sessions: status, loop phase, last activity";
    requirements = [ "fd" "python3" ];
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
