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
import tempfile
import termios
import fcntl
import sys
import threading
import time

BIN = sys.argv[1]
REPO = sys.argv[2]
SESSION = "tui-test"
EXT_SESSION = "tui-test-ext"
WHEEL_UP = b"\x1b[<64;5;5M"  # SGR mouse: wheel up at col 5 row 5


def fixture_dir(name):
    return REPO + "/scripts/ext-fixture/" + name


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


def spawn(session=SESSION, config=None):
    master, slave = pty.openpty()
    winsz = struct.pack("HHHH", 24, 80, 0, 0)
    fcntl.ioctl(master, termios.TIOCSWINSZ, winsz)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, winsz)
    cfg = config if config is not None else REPO + "/config.toml"
    pid = os.fork()
    if pid == 0:
        os.setsid()
        os.dup2(slave, 0)
        os.dup2(slave, 1)
        os.dup2(slave, 2)
        for fd in (master, slave):
            os.close(fd)
        os.execv(BIN, [BIN, session, "--config", cfg])
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


def ext_config(tmpdir, fixture):
    """A temp config that points `[ext] dir` at a fixture layer.

    The sessions root is a private dir so the ext cases never touch
    the repo session list.
    """
    cfg_dir = tmpdir + "/ext-cfg"
    os.makedirs(cfg_dir, exist_ok=True)
    sessions = tmpdir + "/ext-sessions"
    os.makedirs(sessions, exist_ok=True)
    path = cfg_dir + "/config.toml"
    with open(path, "w") as f:
        f.write("[paths]\n")
        f.write(f"sessions_root = \"{sessions}\"\n\n")
        f.write("[ext]\n")
        f.write(f"dir = \"{fixture_dir(fixture)}\"\n")
    return path, sessions


def fixture_orphans(fixture):
    """Pids whose cwd is under a fixture layer.

    The host starts every extension with cwd set to its entry dir
    (docs/ui-extension.md section 7). After the TUI quits, no such
    process may survive.
    """
    prefix = fixture_dir(fixture)
    orphans = []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            cwd = os.readlink(f"/proc/{d}/cwd")
        except OSError:
            continue
        if cwd.startswith(prefix + "/"):
            orphans.append((d, cwd))
    return orphans


def ext_case(name, fixture, expect, wait_seconds, log_check=None):
    """Start the TUI on a fresh session with one fixture layer.

    `expect` is a list of screen substrings to catch within
    `wait_seconds` (each once; later frames may replace the flash).
    `log_check` is an optional sessions dir to scan for a string
    after the quit.
    """
    tmp = tempfile.mkdtemp(prefix="tui-ext-smoke-")
    cfg, sessions = ext_config(tmp, fixture)
    master, pid = spawn(EXT_SESSION, cfg)
    screen = Screen(24, 80)
    try:
        deadline = time.time() + wait_seconds
        seen = set()
        while time.time() < deadline and len(seen) < len(expect):
            pump(master, 0.15, screen)
            if not alive(pid):
                print(f"FAIL {name}: process died while waiting for {expect}")
                return False
            text = screen.text()
            for sub in expect:
                if sub in text:
                    seen.add(sub)
        missing = [s for s in expect if s not in seen]
        if missing:
            print(f"FAIL {name}: markers not seen within {wait_seconds}s: {missing}")
            print("screen was:\n" + screen.text())
            os.kill(pid, signal.SIGKILL)
            reap(pid)
            return False
        # Double-q quit, then prove no orphan fixture process lives.
        os.write(master, b"q")
        pump(master, 0.4, screen)
        os.write(master, b"q")
        deadline = time.time() + 4.0
        while time.time() < deadline and alive(pid):
            pump(master, 0.2, screen)
        if alive(pid):
            print(f"FAIL {name}: still running after double-q (hang)")
            os.kill(pid, signal.SIGKILL)
            reap(pid)
            return False
        reap(pid)
        orphans = fixture_orphans(fixture)
        if orphans:
            print(f"FAIL {name}: orphan fixture processes: {orphans}")
            for d, _ in orphans:
                try:
                    os.kill(int(d), signal.SIGKILL)
                except OSError:
                    pass
            return False
        if log_check:
            log = os.path.join(sessions, EXT_SESSION, "events.jsonl")
            content = ""
            if os.path.exists(log):
                with open(log) as f:
                    content = f.read()
            if log_check not in content:
                print(f"FAIL {name}: session log lacks {log_check!r}")
                print("log was:\n" + content)
                return False
        print(f"OK {name}: markers seen, clean quit, no orphans")
        return True
    finally:
        try:
            os.close(master)
        except OSError:
            pass


def ext_stub_alive():
    """A status extension owns the status row."""
    return ext_case(
        "ext-stub-alive",
        "stub",
        ["EXT stub alive"],
        6.0,
    )


def ext_dying_hint():
    """A dying extension exhausts the 1s/2s/4s budget, then hints."""
    return ext_case(
        "ext-dying-hint",
        "dying",
        ["ext dying dead after 3 restarts"],
        15.0,
    )


def ext_badjsonl():
    """Broken JSONL on every op: the TUI stays alive and quits."""
    return ext_case(
        "ext-badjsonl",
        "badjsonl",
        [" (no events yet"],  # the built-in placeholder still renders
        4.0,
    )


def ext_append_reject():
    """The whitelist reject flashes; the whitelisted append lands."""
    return ext_case(
        "ext-append-reject",
        "append-reject",
        [
            # The full flash text exceeds the 80-col row; assert the
            # visible prefix up to the type name.
            "append rejected: type `tool_result`",
            "ext append-reject appended ext_status",
        ],
        12.0,
        log_check='"type":"ext_status"',
    )


def main():
    ok = True
    ok &= case("baseline-double-q", 0)
    ok &= case("burst-300-then-double-q", 300)
    ok &= case("burst-1000-then-double-q", 1000)
    ok &= scroll_burst_reaches_head()
    ok &= ext_stub_alive()
    ok &= ext_dying_hint()
    ok &= ext_badjsonl()
    ok &= ext_append_reject()
    if not ok:
        sys.exit(1)
    print("ALL SMOKE CASES PASSED")


if __name__ == "__main__":
    main()
