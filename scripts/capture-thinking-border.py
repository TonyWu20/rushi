#!/usr/bin/env python3
r"""capture-thinking-border: the marker-vs-plain SGR comparison.

An application (docs/skill-remapped-to-os-apps.md sections 1-4): a
project-specific, short-lived command on the agent-visible path
(`scripts/`), needed only while the TUI thinking-level border is
under development (`docs/tui-thinking-level-input-box.md`). It is
not in the base distribution. Discovery is on demand: `ls scripts/`,
then this `--help`. There is no SKILL.md: the interface below is
the documentation.

The general pty-capture procedure is the reference application,
`scripts/tui-capture.py`; this script is its two-session
specialization: it compares the SGR color families the input-area
border emits for a marker log against a marker-free log.

WHAT IT DOES
  Runs the built `tui` under a pseudo-terminal against two sessions:
  one whose log holds a `model_thinking` ext_status (value 4, the
  yellow bucket) and one with no marker (the default, gray). It
  classifies every emitted SGR sequence into color families and
  checks: the marker session carries yellow-family codes; the plain
  session carries gray and no yellow. The TUI is SIGKILLed after a
  few frames; the stream, not the quit gate, is under test.
  Exit 0 on PASS, 1 on FAIL.

USAGE
  capture-thinking-border.py [--repo PATH] [--bin PATH]
                            [--marker-session NAME] [--plain-session NAME]
                            [--cols N] [--rows N] [--seconds N]

EXAMPLES
  # the default: repo-local sessions, target/debug/tui:
  capture-thinking-border.py
  # a release binary, custom session names:
  capture-thinking-border.py --bin ../target/release/tui \
      --marker-session think-yellow --plain-session think-gray
"""
import argparse
import fcntl
import os
import pty
import re
import signal
import struct
import sys
import termios
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)

WATCH_SECONDS = 6.0


def capture(bin_path, session, repo, cols, rows, watch_seconds):
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(repo)
        env = dict(os.environ)
        env["TERM"] = "xterm-256color"
        env["COLORTERM"] = "truecolor"
        os.execve(bin_path, [bin_path, session], env)
    winsz = struct.pack("hhhh", rows, cols, 0, 0)
    fcntl.ioctl(master, termios.TIOCSWINSZ, winsz)
    raw = bytearray()
    start = time.monotonic()
    while True:
        r, _, _ = __import__("select").select([master], [], [], 0.05)
        if r:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            raw.extend(chunk)
        if time.monotonic() - start > watch_seconds:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            break
    try:
        os.close(master)
    except OSError:
        pass
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass
    return bytes(raw)


def sgr_families(raw: bytes) -> dict:
    text = raw.decode("utf-8", "replace")
    counts = {}
    for s in re.findall(r"\x1b\[[0-9;:]*m", text):
        counts[s] = counts.get(s, 0) + 1
    fam = {
        "yellow": 0,
        "gray/dark": 0,
        "blue": 0,
        "cyan": 0,
        "green": 0,
    }
    # 256-color codes in the xterm-16 block (0-15): 2 green, 3 yellow,
    # 4 blue, 6 cyan, 8 gray, 10/11/12/14 brights. Sequences may carry
    # modifier suffixes (`38;5;3;49m`): split the color from them.
    for seq, n in counts.items():
        body = seq[2:-1]  # strip ESC [ and m
        if body.startswith("38;5;"):
            color = "38;5;" + body.split(";")[2]
        elif body.startswith("38;2;"):
            parts = body.split(";")
            color = "38;2;" + ";".join(parts[1:4])
        else:
            color = body.split(";")[0]
        if color in ("33", "93", "38;5;3", "38;5;11", "38;5;226", "38;2;255;255;0", "38;2;128;128;0"):
            fam["yellow"] += n
        if color in ("30", "90", "38;5;8", "38;5;235", "38;5;236", "38;5;245", "38;2;128;128;128", "38;2;100;100;100"):
            fam["gray/dark"] += n
        if color in ("34", "38;5;4", "38;5;12", "38;5;33", "38;2;0;0;255"):
            fam["blue"] += n
        if color in ("36", "38;5;6", "38;5;14", "38;5;51", "38;2;0;255;255"):
            fam["cyan"] += n
        if color in ("32", "38;5;2", "38;5;10", "38;5;40", "38;2;0;255;0"):
            fam["green"] += n
    return {
        "counts": counts,
        "families": fam,
    }


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Compare the SGR color families the tui input-area "
                    "border emits for a marker log and a marker-free log.")
    ap.add_argument("--repo", default=REPO,
                    help="the repo root (default: the parent of scripts/)")
    ap.add_argument("--bin", default=None,
                    help="the tui binary to run (default: $repo/target/debug/tui)")
    ap.add_argument("--marker-session", default="scratch-thinking-publish",
                    help="the session with a model_thinking marker in its log")
    ap.add_argument("--plain-session", default="scratch-thinking-none",
                    help="the session with no marker (the default border)")
    ap.add_argument("--cols", type=int, default=100)
    ap.add_argument("--rows", type=int, default=30)
    ap.add_argument("--seconds", type=float, default=WATCH_SECONDS,
                    help="how long to watch each run before SIGKILL")
    args = ap.parse_args()
    if args.bin is None:
        args.bin = os.path.join(args.repo, "target", "debug", "tui")

    # A session with no marker in the log: the default gray border.
    plain_dir = os.path.join(args.repo, "sessions", args.plain_session)
    os.makedirs(plain_dir, exist_ok=True)
    if not os.path.exists(os.path.join(plain_dir, "events.jsonl")):
        with open(os.path.join(plain_dir, "events.jsonl"), "w") as f:
            f.write('{"v":1,"type":"user_message","ts":"2026-09-01T00:00:00Z","content":"hello"}\n')

    marker = sgr_families(capture(args.bin, args.marker_session, args.repo,
                                  args.cols, args.rows, args.seconds))
    plain = sgr_families(capture(args.bin, args.plain_session, args.repo,
                                 args.cols, args.rows, args.seconds))

    for name, cap in [("marker=4 (expect yellow)", marker), ("no marker (expect gray)", plain)]:
        print(f"== {name}: {len(cap['counts'])} distinct SGR sequences")
        for fam, n in cap["families"].items():
            print(f"   {n:>4}  {fam}")
        top = sorted(cap["counts"].items(), key=lambda kv: -kv[1])[:8]
        for seq, n in top:
            print(f"   {n:>4}  {seq[2:-1]}m")

    ok = (
        marker["families"]["yellow"] > 0
        and plain["families"]["yellow"] == 0
        and plain["families"]["gray/dark"] > 0
    )
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
