#!/usr/bin/env python3
"""pty smoke test for the tui binary.

Checks:
1. The TUI starts on a session and quits on double `q`.
2. A burst of SGR mouse-wheel events does not hang the input loop:
   after 300 or 1000 wheel-up events, `q` `q` still exits the process.
3. A large wheel-up burst moves the transcript to the log head.

The screen checks replay the pty byte stream into a small terminal
grid. Ratatui diffs frames: it writes only the cells that changed, so
raw stream text is fragmented. The replayed grid is the real display.
"""
import os
import pty
import select
import signal
import struct
import termios
import fcntl
import sys
import threading
import time

BIN = sys.argv[1]
REPO = sys.argv[2]
SESSION = "tui-test"
WHEEL_UP = b"\x1b[<64;5;5M"  # SGR mouse: wheel up at col 5 row 5


class Screen:
    """A minimal terminal grid that replays the TUI's output.

    Handles the escape codes the crossterm diff writer emits: CUP
    cursor position (`H`), SGR style (`m`, skipped), line erase
    (`K`), clear (`J`), vertical position (`d`), CR/LF, and UTF-8
    printable characters. Private-mode sequences (`?`) are skipped.
    """

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
            if b == 0x1B:  # escape
                if i + 1 < n and data[i + 1] == 0x5B:  # CSI: \x1b[
                    j = i + 2
                    private = False
                    params = b""
                    while j < n:
                        ch = data[j]
                        if 0x30 <= ch <= 0x39 or ch == 0x3B:  # digit or ;
                            params += bytes([ch])
                            j += 1
                        elif ch == 0x3F:  # ? (private mode)
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
                    elif fin == b"K":  # erase to end of line
                        for k in range(self.c, self.cols):
                            self.grid[self.r][k] = " "
                    elif fin == b"J":  # clear display
                        self.grid = [[" "] * self.cols for _ in range(self.rows)]
                    elif fin == b"d":  # vertical position
                        if params and not private:
                            self.r = int(params.decode()) - 1
                            self.r = max(0, min(self.r, self.rows - 1))
                        self.c = 0
                    elif fin == b"n":  # DSR query: the answer flows
                        # back to us on the pty master; ignore it.
                        pass
                    # m and unknowns: nothing to apply
                    i = j + 1
                    continue
                i += 1
                continue
            if b == 0x0A:  # LF
                self.c = 0
                self.r = min(self.r + 1, self.rows - 1)
                i += 1
                continue
            if b == 0x0D:  # CR
                self.c = 0
                i += 1
                continue
            if b >= 0x20:
                if b >= 0x80:  # start of a UTF-8 sequence
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


def spawn():
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
        for fd in (master, slave):
            os.close(fd)
        os.execv(BIN, [BIN, SESSION, "--config", REPO + "/config.toml"])
    os.close(slave)
    return master, pid


def pump(master, seconds, screen):
    """Read pty output for `seconds` and feed it to the screen grid."""
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([master], [], [], 0.1)
        if r:
            try:
                screen.feed(os.read(master, 65536))
            except OSError:
                break


def write_burst(master, data, screen, timeout=60.0):
    """Write a large input burst while draining pty output concurrently.

    The pty output queue holds only a few kilobytes. The TUI blocks
    in its draw call when that queue fills. If the caller stopped
    reading output while the burst write was in flight, the two
    would deadlock each other. A real terminal drains output all
    the time, so this helper mirrors that with a pump loop in the
    calling thread while a worker thread performs the write.
    """
    result = {"done": False}
    def worker():
        try:
            os.write(master, data)
        except OSError:
            pass
        result["done"] = True
    t = threading.Thread(target=worker, daemon=True)
    t.start()
    deadline = time.time() + timeout
    while not result["done"]:
        if time.time() > deadline:
            return False
        pump(master, 0.05, screen)
    return True


def alive(pid):
    try:
        p, _ = os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        # Already reaped by an earlier alive(): the child is gone.
        return False
    return p == 0


def reap(pid):
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass


def case(name, burst):
    master, pid = spawn()
    screen = Screen(24, 80)
    try:
        pump(master, 1.5, screen)  # let the first frame render
        if not alive(pid):
            print(f"FAIL {name}: process died during startup")
            reap(pid)
            return False
        if burst:
            ok = write_burst(master, WHEEL_UP * burst, screen)
            if not ok:
                print(f"FAIL {name}: burst write blocked after timeout")
                return False
            pump(master, 0.5, screen)  # let the burst be processed
        os.write(master, b"q")
        pump(master, 0.4, screen)
        os.write(master, b"q")
        deadline = time.time() + 3.0
        while time.time() < deadline:
            if not alive(pid):
                break
            pump(master, 0.2, screen)
        if alive(pid):
            print(f"FAIL {name}: still running after double-q (hang)")
            os.kill(pid, signal.SIGKILL)
            reap(pid)
            return False
        reap(pid)
        print(f"OK {name}: exited after double-q" + (f" (burst={burst})" if burst else ""))
        return True
    finally:
        try:
            os.close(master)
        except OSError:
            pass


def scroll_burst_reaches_head():
    """A large wheel-up burst must move the transcript to the log head.

    The TUI opens in follow-tail mode: the tail of the session log is
    visible. 2000 wheel-up events scroll 6000 lines, far past the
    head of the tui-test log.
    """
    HEAD = "This is the first time"
    # Near the very end of the last event: the tail viewport shows it.
    TAIL = "Want me to fix #1 and #2"
    # 30000 events x 3 lines = 90000 lines: past the log head and
    # safely under the 100000-line scroll cap.
    BURST = 30000
    master, pid = spawn()
    screen = Screen(24, 80)
    try:
        pump(master, 1.5, screen)
        if not alive(pid):
            print("FAIL scroll-burst: process died during startup")
            reap(pid)
            return False
        initial = screen.text()
        if TAIL not in initial:
            print("FAIL scroll-burst: tail marker missing at startup")
            print("screen was:\n" + initial)
            return False
        if HEAD in initial:
            print("FAIL scroll-burst: head already visible at startup")
            return False
        ok = write_burst(master, WHEEL_UP * BURST, screen)
        if not ok:
            print("FAIL scroll-burst: burst write blocked after timeout")
            return False
        pump(master, 1.5, screen)
        if not alive(pid):
            print("FAIL scroll-burst: process died during the burst")
            reap(pid)
            return False
        after = screen.text()
        if HEAD in after:
            os.kill(pid, signal.SIGTERM)
            reap(pid)
            print("OK scroll-burst-reaches-head: wheel burst reached the log head")
            return True
        print("FAIL scroll-burst: head marker not visible after burst")
        print("screen was:\n" + after)
        os.kill(pid, signal.SIGKILL)
        reap(pid)
        return False
    finally:
        try:
            os.close(master)
        except OSError:
            pass


def main():
    ok = True
    ok &= case("baseline-double-q", 0)
    ok &= case("burst-300-then-double-q", 300)
    ok &= case("burst-1000-then-double-q", 1000)
    ok &= scroll_burst_reaches_head()
    if not ok:
        sys.exit(1)
    print("ALL SMOKE CASES PASSED")


if __name__ == "__main__":
    main()
