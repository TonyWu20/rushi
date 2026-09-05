#!/usr/bin/env python3
"""Capture the raw bytes the tui binary writes to a PTY.

Run the built tui under a pseudo-terminal, let it draw a few frames,
quit it with q q, and dump every byte the terminal ever saw. The
result is classified: every SGR (CSI ... m) sequence, its count, and
which color families appear (16-color SGR, 256-color, truecolor).
"""
import fcntl
import json
import os
import re
import select
import signal
import struct
import sys
import termios
import time

import pty

ROOT = "/home/tony/programming/rust-unix-harness"
BIN = os.path.join(ROOT, "target/debug/tui")
SESSION = "color-capture"
DUMP = os.path.join(ROOT, "scratch/color-capture-raw.bin")

COLS, ROWS = 100, 30
WATCH_SECONDS = 8.0


def main() -> int:
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(ROOT)
        env = dict(os.environ)
        env["TERM"] = "xterm-256color"
        env["COLORTERM"] = "truecolor"  # what a modern emulator would advertise
        os.execve(BIN, [BIN, SESSION], env)
    # parent
    winsz = struct.pack("hhhh", ROWS, COLS, 0, 0)
    fcntl.ioctl(master, termios.TIOCSWINSZ, winsz)

    raw = bytearray()
    last_write = time.monotonic()
    sent_quit = False
    start = time.monotonic()
    last_chunk = 0.0
    while True:
        now = time.monotonic()
        r, _, _ = select.select([master], [], [], 0.05)
        if r:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            raw.extend(chunk)
            last_chunk = now
        # After quiet frames + the first redraw burst, quit the TUI.
        if not sent_quit and now - start > WATCH_SECONDS:
            os.write(master, b"q")
            time.sleep(0.15)
            os.write(master, b"q")
            sent_quit = True
        # Once the TUI quit and the stream goes quiet, stop.
        if sent_quit and (time.monotonic() - last_chunk > 3.0 or len(raw) > 4_000_000):
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            break
        if time.monotonic() - start > 30.0:
            os.kill(pid, signal.SIGKILL)
            break
    try:
        os.close(master)
    except OSError:
        pass
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass

    with open(DUMP, "wb") as f:
        f.write(bytes(raw))

    # ── classify the emitted color sequences ──
    text = raw.decode("utf-8", "replace")
    sgr = re.findall(r"\x1b\[([0-9;:]*[A-Za-z]?)m", text)
    sgr = [s + "m" for s in sgr if s.endswith("m")]
    counts: dict[str, int] = {}
    for s in sgr:
        counts[s] = counts.get(s, 0) + 1
    families = {
        "reset(0)": ["\x1b[0m"],
        "default-fg/bg(39/49)": [c for c in counts if re.fullmatch(r"\x1b\[?(?:39|49);?\d*?m", c[2:]) or True],
    }
    has_256 = any(re.search(r"(?:38|48);5;", c[2:]) for c in counts)
    has_true = any(re.search(r"(?:38|48);2;", c[2:]) for c in counts)
    has_16 = any(re.search(r"\x1b\[[0-7;]*[0-5][09]m", c) or re.search(r"\x1b\[[0-9;]*;?(?:3[0-7]|4[0-7]|5[0-7]|9[0-9]|10[0-9])m", c) for c in counts)

    print(f"captured {len(raw)} bytes -> {DUMP}")
    print(f"distinct SGR sequences: {len(counts)}; 256-color codes: {has_256}; truecolor codes: {has_true}; 16-color codes: {has_16}")
    print("top SGR sequences by count:")
    for seq, n in sorted(counts.items(), key=lambda kv: -kv[1])[:40]:
        pretty = seq.replace("\x1b", "\\e")
        print(f"  {n:>6}  {pretty}")


if __name__ == "__main__":
    sys.exit(main())
