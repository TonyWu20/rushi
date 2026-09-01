#!/usr/bin/env python3
"""Capture the SGR codes the tui emits for the input-area border.

Run the built tui under a pseudo-terminal against two sessions:
one whose log holds a `model_thinking` ext_status (value 4, the
yellow bucket) and one with no marker (the default, gray). Compare
the emitted color sequences: the marker session must carry the
yellow-family codes the other lacks. The TUI is SIGKILLed after a
few frames; the stream, not the quit gate, is under test.
"""
import fcntl
import os
import pty
import re
import signal
import struct
import sys
import termios
import time

ROOT = "/home/tony/programming/rust-unix-harness"
BIN = os.path.join(ROOT, "target/debug/tui")
MARKER_SESSION = "scratch-thinking-publish"
PLAIN_SESSION = "scratch-thinking-none"
COLS, ROWS = 100, 30
WATCH_SECONDS = 6.0


def capture(session: str) -> bytes:
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(ROOT)
        env = dict(os.environ)
        env["TERM"] = "xterm-256color"
        env["COLORTERM"] = "truecolor"
        os.execve(BIN, [BIN, session], env)
    winsz = struct.pack("hhhh", ROWS, COLS, 0, 0)
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
        if time.monotonic() - start > WATCH_SECONDS:
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
    # A session with no marker in the log: the default gray border.
    plain_dir = os.path.join(ROOT, "sessions", PLAIN_SESSION)
    os.makedirs(plain_dir, exist_ok=True)
    if not os.path.exists(os.path.join(plain_dir, "events.jsonl")):
        with open(os.path.join(plain_dir, "events.jsonl"), "w") as f:
            f.write('{"v":1,"type":"user_message","ts":"2026-09-01T00:00:00Z","content":"hello"}\n')

    marker = sgr_families(capture(MARKER_SESSION))
    plain = sgr_families(capture(PLAIN_SESSION))

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
