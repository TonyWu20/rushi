#!/usr/bin/env python3
"""Probe 3: command-line prompt when a frame extension owns the label.

A temp ext layer holds only the `frame` extension. It labels the
input frame with the editor mode, so the built-in title (the prompt
in command-line mode) is replaced. Check whether the typed prompt
stays visible.
"""
import os
import pty
import select
import struct
import sys
import tempfile
import termios
import fcntl
import time

BIN = sys.argv[1]
REPO = sys.argv[2]
SESSION = sys.argv[3] if len(sys.argv) > 3 else "e2e"


class Screen:
    def __init__(self, rows, cols):
        self.rows = rows
        self.cols = cols
        self.grid = [[" "] * cols for _ in range(rows)]
        self.r = 0
        self.c = 0

    def feed(self, data):
        i = 0
        n = len(data)
        while i < n:
            b = data[i]
            if b == 0x1B:
                if i + 1 < n and data[i + 1] == 0x5B:
                    j = i + 2
                    private = False
                    params = b""
                    while j < n:
                        ch = data[j]
                        if 0x30 <= ch <= 0x39 or ch == 0x3B:
                            params += bytes([ch])
                            j += 1
                        elif ch == 0x3F:
                            private = True
                            j += 1
                        else:
                            break
                    fin = data[j:j + 1]
                    if fin == b"H" or fin == b"F":
                        p = params.decode().split(";")
                        self.r = int(p[0]) - 1 if p[0] else 0
                        self.c = int(p[1]) - 1 if len(p) > 1 and p[1] else 0
                        self.r = max(0, min(self.r, self.rows - 1))
                        self.c = max(0, min(self.c, self.cols - 1))
                    elif fin == b"K":
                        for k in range(self.c, self.cols):
                            self.grid[self.r][k] = " "
                    elif fin == b"J":
                        self.grid = [[" "] * self.cols for _ in range(self.rows)]
                    elif fin == b"d":
                        if params and not private:
                            self.r = int(params.decode()) - 1
                            self.r = max(0, min(self.r, self.rows - 1))
                        self.c = 0
                    i = j + 1
                    continue
                i += 1
                continue
            if b == 0x0A:
                self.c = 0
                self.r = min(self.r + 1, self.rows - 1)
                i += 1
                continue
            if b == 0x0D:
                self.c = 0
                i += 1
                continue
            if b >= 0x20:
                if b >= 0x80:
                    need = 2 if b < 0xE0 else 3 if b < 0xF0 else 4
                    chunk = data[i:i + need].decode("utf-8", errors="replace")
                    if self.r < self.rows and self.c < self.cols:
                        self.grid[self.r][self.c] = chunk[0]
                    i += need
                else:
                    if self.r < self.rows and self.c < self.cols:
                        self.grid[self.r][self.c] = chr(b)
                    i += 1
                self.c = min(self.c + 1, self.cols - 1)
                continue
            i += 1

    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.grid)


def main():
    tmp = tempfile.mkdtemp(prefix="frame-probe-")
    ext_dir = tmp + "/ext"
    os.makedirs(ext_dir, exist_ok=True)
    src = os.path.realpath(REPO + "/ui_extensions/frame")
    os.symlink(src, ext_dir + "/frame")
    sessions = tmp + "/sessions"
    os.makedirs(sessions)
    cfg = tmp + "/config.toml"
    with open(cfg, "w") as f:
        f.write(f"[paths]\nsessions_root = \"{REPO}/sessions\"\n")
        f.write(f'[ext]\ndir = "{ext_dir}"\n')

    master, slave = pty.openpty()
    winsz = struct.pack("HHHH", 24, 80, 0, 0)
    fcntl.ioctl(master, termios.TIOCSWINSZ, winsz)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, winsz)
    pid = os.fork()
    if pid == 0:
        os.setsid()
        os.dup2(slave, 0)
        os.dup2(slave, 1)
        os.dup2(slave, 2)
        os.close(master)
        os.close(slave)
        os.execv(BIN, [BIN, SESSION, "--config", cfg])
    os.close(slave)

    screen = Screen(24, 80)

    def pump(seconds):
        end = time.time() + seconds
        while time.time() < end:
            r, _, _ = select.select([master], [], [], 0.1)
            if r:
                try:
                    data = os.read(master, 65536)
                    if data:
                        screen.feed(data)
                except OSError:
                    break

    try:
        pump(3.0)  # first frame plus the ext frame reply
        os.write(master, b"first line")
        pump(0.4)
        os.write(master, b"\x1b")
        pump(0.3)
        os.write(master, b"/")
        pump(0.3)
        os.write(master, b"ab")
        pump(1.5)  # let the frame extension tick with the new mode
        text = screen.text()
        print("=== screen after `/ab` with the frame extension ===")
        print(text)
        print("=== checks ===")
        for marker in ["/ab", "/a", "█", "[COMMAND]"]:
            print(f"  {marker!r} visible: {marker in text}")
        import signal
        os.kill(pid, signal.SIGTERM)
    finally:
        try:
            os.close(master)
        except OSError:
            pass
        os.waitpid(pid, 0)


if __name__ == "__main__":
    main()
