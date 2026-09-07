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

Args: the `tui` binary, then the kernel repo root. The repo root is
used for the plain cases' session (`sessions/tui-test`, a
machine-local fixture: the head/tail markers the scroll-burst case
asserts), the `scripts/ext-fixture/` host-test inputs, and the
default extension layer tree. The `EXTS_ROOT` env var points the
extension layer tree (`ui_extensions/`, `ext-rs/`,
`ui_extensions-demos/`) at a separate exts checkout
(docs/tui-ext-repo-split.md section 4, item 4); the default is the
repo argument.
"""
import os
import pty
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import fcntl
import json
import sys
import threading
import time

BIN = sys.argv[1]
# Absolute: the /proc helpers compare the absolute `/proc/*/cwd`
# readlinks against the layer prefixes. A relative repo path
# (the `.` of a direct run) would match nothing and the orphan
# checks would silently no-op.
REPO = os.path.abspath(sys.argv[2])
# Root of the extension layer tree (ui_extensions/, ext-rs/,
# ui_extensions-demos/). Same-tree default: the kernel repo
# argument. A split layout (docs/tui-ext-repo-split.md) points it
# at the exts repo checkout via the EXTS_ROOT env var; kernel-side
# paths (sessions, the ext-fixture host-test inputs, temp configs)
# stay under REPO.
EXTS_ROOT = os.path.abspath(os.environ.get("EXTS_ROOT", REPO))
SESSION = "tui-test"
EXT_SESSION = "tui-test-ext"
MERMAID_EXT_DIR = EXTS_ROOT + "/ui_extensions/mermaid"
GOAL_EXT_DIR = EXTS_ROOT + "/ui_extensions/goal"
EXT_RS_STATUSLINE = EXTS_ROOT + "/ext-rs/statusline-rs"
EXT_RS_TOOL_RESULT = EXTS_ROOT + "/ext-rs/tool_result-rs"
EXT_RS_NOTIFY = EXTS_ROOT + "/ext-rs/notify-rs"
WHEEL_UP = b"\x1b[<64;5;5M"  # SGR mouse: wheel up at col 5 row 5


def setup_ext_bins():
    """Build the Rust extension binaries and put them on PATH.

    The Rust reference extensions are standalone cargo packages
    (ui-extension-plan stages 3 and 4). Their manifests resolve the
    binary names on PATH, and the host refuses the start when a
    command is missing (docs/ui-extension.md section 6). Every case
    in this file loads a layer that includes them, so the binaries
    must exist and be reachable before the first spawn.
    """
    import subprocess
    ext_packages = [
        (MERMAID_EXT_DIR, "mermaid-ext"),
        (GOAL_EXT_DIR, "goal-ext"),
        (EXT_RS_STATUSLINE, "statusline-ext"),
        (EXT_RS_TOOL_RESULT, "tool_result-ext"),
        (EXT_RS_NOTIFY, "notify-ext"),
    ]
    for ext_dir, bin_name in ext_packages:
        bin_dir = ext_dir + "/target/debug"
        bin_path = bin_dir + "/" + bin_name
        if not os.path.exists(bin_path):
            r = subprocess.run(
                ["cargo", "build"],
                cwd=ext_dir,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
            )
            if r.returncode != 0:
                print(f"FAIL setup: cannot build {ext_dir}:")
                print(r.stderr.decode())
                sys.exit(1)
        if bin_dir not in os.environ.get("PATH", "").split(os.pathsep):
            os.environ["PATH"] = bin_dir + os.pathsep + os.environ.get("PATH", "")


def fixture_dir(name):
    # Absolute: the temp config lives outside the repo, and a
    # relative dir would resolve against the config's directory.
    # The fixture layers are host-test inputs (they exercise the
    # TUI host against broken extensions), so they stay under the
    # kernel REPO even when EXTS_ROOT points at a separate exts
    # checkout; at the split (docs/tui-ext-repo-split.md) they
    # move to the TUI repo as `scripts/ext-fixture/`.
    return os.path.abspath(REPO + "/scripts/ext-fixture/" + name)


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
        # The raw byte stream. The grid loses control bytes (BEL, OSC);
        # the raw buffer keeps them for assertions like "the turn
        # finished and the terminal bell rang".
        self.raw = bytearray()

    def feed(self, data):
        self.raw += data
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


def spawn(session=SESSION, config=None, prepend_path=None):
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
        if prepend_path:
            os.environ["PATH"] = (
                prepend_path + os.pathsep + os.environ.get("PATH", "")
            )
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
        # The vim modal composer starts in insert mode: the quit gate
        # (q q in normal mode with an empty draft) needs the insert
        # exit key first. Esc drops to normal without touching the
        # draft, so the draft stays empty and the gate can fire.
        os.write(master, b"\x1b")
        pump(master, 0.3, screen)
        os.write(master, b"q")
        pump(master, 0.4, screen)
        os.write(master, b"q")
        # The quit path runs the stop sequence with a 3 s
        # SIGTERM->SIGKILL grace (docs/ui-extension.md section 7);
        # on a slow box the exit measures ~4.5 s, so the budget must
        # exceed the grace with margin. 3.0 s was systematically too
        # tight and turned a healthy TUI into a false "hang"
        # (2026-09-07: the three plain cases failed while the TUI
        # exited cleanly at ~4.5 s after the second q).
        deadline = time.time() + 8.0
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
    """Orphan pids whose cwd is under a fixture layer.

    The host starts every extension with cwd set to its entry dir
    (docs/ui-extension.md section 7). After the TUI quits, no such
    process may survive. Only pids reparented to init count: a
    live TUI still runs its extension set under the same dirs.
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
        if not cwd.startswith(prefix + "/"):
            continue
        if proc_ppid(d) != 1:
            continue
        orphans.append((d, cwd))
    return orphans


def ext_case_cleanup(pid, fixture):
    """Failure-path cleanup for the fixture cases.

    SIGKILL the TUI process group, then SIGKILL the fixture layer
    pids (the extension processes survive an abnormal TUI death:
    each one does its own setsid, docs/ui-extension.md section 7).
    """
    try:
        os.kill(-pid, signal.SIGKILL)
    except OSError:
        pass
    time.sleep(0.3)
    for d, _ in fixture_orphans(fixture):
        try:
            os.kill(int(d), signal.SIGKILL)
        except OSError:
            pass


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
                ext_case_cleanup(pid, fixture)
                return False
            text = screen.text()
            for sub in expect:
                if sub in text:
                    seen.add(sub)
        missing = [s for s in expect if s not in seen]
        if missing:
            print(f"FAIL {name}: markers not seen within {wait_seconds}s: {missing}")
            print("screen was:\n" + screen.text())
            ext_case_cleanup(pid, fixture)
            return False
        # Double-q quit, then prove no orphan fixture process lives.
        # The vim modal composer starts in insert mode: the quit gate
        # (q q in normal mode with an empty draft) needs the insert
        # exit key first (the same fix as the base cases).
        os.write(master, b"\x1b")
        pump(master, 0.3, screen)
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
            ext_case_cleanup(pid, fixture)
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


def ext_frame_commandline():
    """The search prompt stays visible under a frame extension.

    The fixture frame labels the input frame with the editor mode.
    In command-line mode the host renders its own prompt in the box
    title instead of the frame label (docs/ui-extension.md section
    10): the typed pattern must show even when an extension owns
    the chrome. The frame label returns when the search ends.
    """
    tmp = tempfile.mkdtemp(prefix="tui-frame-smoke-")
    cfg, sessions = ext_config(tmp, "frame")
    master, pid = spawn("tui-test-frame", cfg)
    screen = Screen(24, 80)
    try:
        # 1. The frame extension owns the title: its distinctive
        # `fx` label shows in the input box border.
        deadline = time.time() + 6.0
        while time.time() < deadline:
            pump(master, 0.2, screen)
            if not alive(pid):
                print("FAIL ext-frame-commandline: process died at startup")
                return False
            if "fx-[INSERT]" in screen.text():
                break
        if "fx-[INSERT]" not in screen.text():
            print("FAIL ext-frame-commandline: the fixture frame label never showed")
            print("screen was:\n" + screen.text())
            return False
        # 2. Normal mode, then the search prompt: Esc, /, ab.
        os.write(master, b"\x1b")
        pump(master, 0.4, screen)
        os.write(master, b"/")
        pump(master, 0.4, screen)
        os.write(master, b"ab")
        pump(master, 1.0, screen)
        text = screen.text()
        if "/ab" not in text or "\u2588" not in text:
            print("FAIL ext-frame-commandline: the typed prompt is not visible under the frame label")
            print("screen was:\n" + text)
            return False
        # 3. Esc cancels the search: the fixture label comes back.
        os.write(master, b"\x1b")
        deadline = time.time() + 4.0
        while time.time() < deadline:
            pump(master, 0.2, screen)
            if "fx-[NORMAL]" in screen.text():
                break
        if "fx-[NORMAL]" not in screen.text():
            print("FAIL ext-frame-commandline: the fixture label did not return after the search")
            print("screen was:\n" + screen.text())
            return False
        # 4. Double-q quit: the draft is empty and the mode is
        # normal, so the quit gate is open.
        os.write(master, b"q")
        pump(master, 0.4, screen)
        os.write(master, b"q")
        deadline = time.time() + 4.0
        while time.time() < deadline and alive(pid):
            pump(master, 0.2, screen)
        if alive(pid):
            print("FAIL ext-frame-commandline: still running after double-q (hang)")
            os.kill(pid, signal.SIGKILL)
            reap(pid)
            return False
        reap(pid)
        orphans = fixture_orphans("frame")
        if orphans:
            print("FAIL ext-frame-commandline: orphan fixture processes:")
            for p, cwd in orphans:
                print(f"  {p} {cwd}")
                try:
                    os.kill(int(p), signal.SIGKILL)
                except OSError:
                    pass
            return False
        print("OK ext-frame-commandline: prompt visible under the frame label, label returns, clean quit")
        return True
    finally:
        try:
            os.close(master)
        except OSError:
            pass
        try:
            import shutil

            shutil.rmtree(tmp, ignore_errors=True)
        except Exception:
            pass


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


def layer_config(tmpdir, layer_dir, active_model=None):
    """A temp config that points `[ext] dir` at a layer directory.

    The layer is usually the repo's `ui_extensions/` global layer
    (the reference extensions) or `ext-rs/`. The sessions root is a
    private dir so the cases never touch the repo session list.
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
        f.write(f"dir = \"{layer_dir}\"\n")
        if active_model:
            f.write("\n[active]\n")
            f.write(f"model = \"{active_model}\"\n")
    return path, sessions


def seed_session(sessions_dir, name, events):
    """Write a session log directly, oldest first."""
    d = os.path.join(sessions_dir, name)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "events.jsonl"), "w") as f:
        for e in events:
            f.write(json.dumps(e) + "\n")


def seed_events():
    """One realistic turn: user, assistant (tool_calls, usage),
    tool_call, tool_result, and a finished assistant turn (usage).
    The usage totals are in:125 out:55 sum:180.
    """
    ts = "2026-08-27T10:00:00Z"
    return [
        {"v": 1, "type": "user_message", "ts": ts, "content": "check the build"},
        {
            "v": 1, "type": "assistant_message", "ts": ts,
            "content": "Running the build now.",
            "tool_calls": [{"id": "call_1", "name": "bash", "arguments": {"command": "make"}}],
            "stop_reason": "tool_calls",
            "usage": {"input_tokens": 100, "output_tokens": 50},
        },
        {"v": 1, "type": "tool_call", "ts": ts, "id": "call_1", "name": "bash", "arguments": {"command": "make"}},
        {"v": 1, "type": "tool_result", "ts": ts, "id": "call_1", "value": {"text": "all good"}, "is_error": False},
        {
            "v": 1, "type": "assistant_message", "ts": ts,
            "content": "Build finished without errors.",
            "tool_calls": [],
            "stop_reason": "stop",
            "usage": {"input_tokens": 25, "output_tokens": 5},
        },
    ]


def proc_ppid(pid):
    """The ppid of one pid, or -1 when the pid is gone."""
    try:
        stat = open(f"/proc/{pid}/stat").read()
        return int(stat.split(") ")[-1].split()[1])
    except (OSError, IndexError, ValueError):
        return -1


def procs_with_cwd_under(prefix, any_parent=False):
    """Pids whose cwd is `prefix` itself or under it.

    The host starts every extension with cwd set to its entry dir
    (docs/ui-extension.md section 7). By default only true orphans
    count: a live TUI still runs its own extension set under the
    same dirs, so a process with a live parent (ppid != 1) is not
    an orphan and must never be flagged or killed. A kill step
    that targets one private layer may scan live children too:
    pass `any_parent=True`.
    """
    pids = []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            cwd = os.readlink(f"/proc/{d}/cwd")
        except OSError:
            continue
        if not (cwd == prefix or cwd.startswith(prefix + "/")):
            continue
        if not any_parent and proc_ppid(d) != 1:
            continue
        pids.append(int(d))
    return pids


def settled_orphans(prefix, seconds=6.0):
    """Orphans after a settle window.

    The host's stop is SIGTERM, then SIGKILL: a signalled extension
    dies a moment after the scan, not before it. A one-shot scan
    false-positives on the dying set. Retry until the pids clear or
    the window runs out; pids that survive the window are real
    orphans (they survived SIGTERM and SIGKILL)."""
    end = time.time() + seconds
    while True:
        pids = procs_with_cwd_under(prefix)
        if not pids or time.time() >= end:
            return pids
        time.sleep(0.5)


def wait_markers(master, pid, screen, markers, deadline, grace=10.0):
    """Pump until every marker is seen on screen or the deadline.

    When the deadline passes with the TUI still alive, the wait
    extends once by `grace` seconds: under heavy machine load the
    extension spawn or restart can land just after the deadline. A
    dead TUI fails at the first deadline, without the grace.
    """
    seen = set()
    extended = False
    while time.time() < deadline and len(seen) < len(markers):
        pump(master, 0.15, screen)
        if not alive(pid):
            return seen, False
        text = screen.text()
        for m in markers:
            if m in text:
                seen.add(m)
    if len(seen) < len(markers) and alive(pid) and not extended:
        extended = True
        deadline += grace
        while time.time() < deadline and len(seen) < len(markers):
            pump(master, 0.15, screen)
            if not alive(pid):
                break
            text = screen.text()
            for m in markers:
                if m in text:
                    seen.add(m)
    return seen, all(m in screen.text() for m in markers) and alive(pid)


def cleanup_layer(pid, layer_prefix):
    """Failure-path cleanup for the ext cases.

    SIGKILL the TUI process group, then SIGKILL every process
    whose cwd is under the layer prefix. The extension processes
    survive the TUI death: each one does its own setsid (the
    host's stop kills them, an abnormal TUI death does not).
    Without this, a leaked TUI plus its extensions accumulates
    across the cases and makes the later orphan check fail.
    """
    try:
        os.kill(-pid, signal.SIGKILL)
    except OSError:
        pass
    time.sleep(0.5)
    orphans = settled_orphans(layer_prefix, 6.0)
    for p in orphans:
        try:
            os.kill(p, signal.SIGKILL)
        except OSError:
            pass


def ext_statusline_real():
    """A real session on the reference layer.

    The statusline shows live dir, git, model, and usage stats.
    The two-line layout shows on the 80-col smoke pty. Restarting the
    TUI mid-session keeps the stats: they are recomputed from the
    log. The finished turn rings the terminal (raw BEL in the
    stream).
    """
    tmp = tempfile.mkdtemp(prefix="tui-ext-real-")
    cfg, sessions = layer_config(tmp, EXTS_ROOT + "/ui_extensions", active_model="smoke-model")
    seed_session(sessions, EXT_SESSION, seed_events())
    # The powerline footer splits the model pill and the stats pill,
    # so the two markers are separate.
    stats_marker = "in:125 out:55 sum:180"
    ok = True
    master, pid = spawn(EXT_SESSION, cfg)
    screen = Screen(24, 80)
    try:
        deadline = time.time() + 15.0
        seen, _ = wait_markers(
            master, pid, screen,
            ["git:none", "smoke-model", stats_marker],
            deadline,
        )
        if not alive(pid):
            print("FAIL ext-statusline-real: process died during startup")
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        missing = [m for m in ["git:none", "smoke-model", stats_marker] if m not in seen]
        if missing:
            print(f"FAIL ext-statusline-real: markers not seen: {missing}")
            print("screen was:\n" + screen.text())
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        # The finished turn rings: the notify extension flushes its
        # remembered turn ~5 s after start. Check the raw stream for
        # the bell and the OSC title.
        deadline = time.time() + 6.0
        while time.time() < deadline:
            pump(master, 0.25, screen)
            if b"\x07" in screen.raw:
                break
        if b"\x07" not in screen.raw:
            print("FAIL ext-statusline-real: no terminal bell in the pty stream")
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        if b"turn finished" not in screen.raw:
            print("FAIL ext-statusline-real: no OSC title in the pty stream")
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        # Quit, then restart: the usage totals must survive from the
        # log alone (the host re-sends every usage-bearing message).
        # The vim modal composer starts in insert mode: the quit gate
        # needs the insert exit key first.
        os.write(master, b"\x1b")
        pump(master, 0.3, screen)
        os.write(master, b"q")
        pump(master, 0.4, screen)
        os.write(master, b"q")
        deadline = time.time() + 4.0
        while time.time() < deadline and alive(pid):
            pump(master, 0.2, screen)
        if alive(pid):
            print("FAIL ext-statusline-real: still running after double-q (hang)")
            os.kill(pid, signal.SIGKILL)
            reap(pid)
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        reap(pid)
        orphans = settled_orphans(EXTS_ROOT + "/ui_extensions")
        if orphans:
            print(f"FAIL ext-statusline-real: orphan layer processes: {orphans}")
            for p in orphans:
                try:
                    os.kill(p, signal.SIGKILL)
                except OSError:
                    pass
            return False
        master2, pid2 = spawn(EXT_SESSION, cfg)
        screen2 = Screen(24, 80)
        try:
            deadline = time.time() + 15.0
            while time.time() < deadline:
                pump(master2, 0.2, screen2)
                if not alive(pid2):
                    print("FAIL ext-statusline-real: restart died during startup")
                    cleanup_layer(pid2, EXTS_ROOT + "/ui_extensions")
                    return False
                if stats_marker in screen2.text():
                    break
            if stats_marker not in screen2.text():
                print("FAIL ext-statusline-real: usage stats did not survive the restart")
                print("screen was:\n" + screen2.text())
                cleanup_layer(pid2, EXTS_ROOT + "/ui_extensions")
                return False
        finally:
            os.write(master2, b"q")
            pump(master2, 0.4, screen2)
            os.write(master2, b"q")
            deadline = time.time() + 4.0
            while time.time() < deadline and alive(pid2):
                pump(master2, 0.2, screen2)
            if alive(pid2):
                os.kill(pid2, signal.SIGKILL)
            reap(pid2)
            cleanup_layer(pid2, EXTS_ROOT + "/ui_extensions")
            try:
                os.close(master2)
            except OSError:
                pass
        print("OK ext-statusline-real: dir/git/model/usage shown, two-line layout, bell, stats survive restart")
        return ok
    finally:
        try:
            os.close(master)
        except OSError:
            pass


def active_model_from_config(path):
    """The [active] model from a config.toml, or None.

    A tiny TOML scan: find the [active] section, then its model key.
    No TOML dependency in the smoke script.
    """
    try:
        with open(path) as f:
            lines = f.read().splitlines()
    except OSError:
        return None
    in_active = False
    for line in lines:
        s = line.strip()
        if s.startswith("["):
            in_active = s.strip("[] ") == "active"
            continue
        if in_active and s.startswith("model"):
            i = s.find('"')
            j = s.find('"', i + 1) if i >= 0 else -1
            if 0 <= i and j > i:
                return s[i + 1 : j]
    return None


def ext_statusline_repo():
    """The repo config on a real session: the global layer loads the
    reference extensions. The statusline shows the live dir, the git
    branch, the active model, and the usage totals from the real log.

    The markers are computed from the checkout, not hardcoded: the
    dir is the repo basename, the branch is the repo branch, and the
    model is the config's [active] model. The row truncates the dir
    to its last 16 chars, so the dir marker is the basename's last
    12. A missing value drops its marker, so the case passes on any
    branch or config."""
    cfg = REPO + "/config.toml"
    master, pid = spawn(SESSION, cfg)
    screen = Screen(24, 80)
    markers = [os.path.basename(REPO)[-12:]]
    branch = subprocess.run(
        ["git", "-C", REPO, "branch", "--show-current"],
        capture_output=True,
        text=True,
    ).stdout.strip()
    if branch:
        markers.append(f"git:{branch}")
    model = active_model_from_config(cfg)
    if model:
        markers.append(model)
    try:
        deadline = time.time() + 15.0
        seen, _ = wait_markers(master, pid, screen, markers, deadline)
        if not alive(pid):
            print("FAIL ext-statusline-repo: process died during startup")
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        text = screen.text()
        missing = [m for m in markers if m not in seen]
        if missing:
            print(f"FAIL ext-statusline-repo: markers not seen: {missing}")
            print("screen was:\n" + text)
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        if "in:" not in text:
            print("FAIL ext-statusline-repo: usage totals not shown")
            print("screen was:\n" + text)
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        # The vim modal composer starts in insert mode: the quit gate
        # needs the insert exit key first.
        os.write(master, b"\x1b")
        pump(master, 0.3, screen)
        os.write(master, b"q")
        pump(master, 0.4, screen)
        os.write(master, b"q")
        deadline = time.time() + 4.0
        while time.time() < deadline and alive(pid):
            pump(master, 0.2, screen)
        if alive(pid):
            print("FAIL ext-statusline-repo: still running after double-q (hang)")
            os.kill(pid, signal.SIGKILL)
            reap(pid)
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        reap(pid)
        orphans = settled_orphans(EXTS_ROOT + "/ui_extensions")
        if orphans:
            print(f"FAIL ext-statusline-repo: orphan layer processes: {orphans}")
            for p in orphans:
                try:
                    os.kill(p, signal.SIGKILL)
                except OSError:
                    pass
            return False
        print("OK ext-statusline-repo: dir, git branch, model, usage on a real session")
        return True
    finally:
        try:
            os.close(master)
        except OSError:
            pass


def ext_statusline_slowgit():
    """The reference statusline under a slow git.

    A PATH wrapper makes every git call take 4 s (a cold cache).
    The row shows within 4 s, and the stale hint never appears while
    a refresh is in flight (a synchronous tick handler crosses the
    3 x tick_ms bound and shows the hint, which is the regression
    this case guards).
    """
    tmp = tempfile.mkdtemp(prefix="tui-ext-slowgit-")
    cfg, sessions = layer_config(tmp, EXTS_ROOT + "/ui_extensions", active_model="smoke-model")
    seed_session(sessions, EXT_SESSION, seed_events())
    bindir = tmp + "/slowbin"
    os.makedirs(bindir)
    real_git = shutil.which("git")
    if not real_git:
        print("FAIL ext-statusline-slowgit: no system git for the wrapper")
        return False
    with open(bindir + "/git", "w") as f:
        f.write('#!/bin/sh\nsleep 4\nexec "' + real_git + '" "$@"\n')
    os.chmod(bindir + "/git", 0o755)
    # The powerline footer splits the model pill and the stats pill,
    # so the row marker is the stats pill text alone.
    stats_marker = "in:125 out:55 sum:180"
    master, pid = spawn(EXT_SESSION, cfg, prepend_path=bindir)
    screen = Screen(24, 80)
    raw = b""
    row_at = None
    stale_at = None
    t0 = time.time()
    end = t0 + 18.0
    try:
        while time.time() < end:
            r, _, _ = select.select([master], [], [], 0.25)
            if r:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                raw += chunk
                screen.feed(chunk)
            if not alive(pid):
                print("FAIL ext-statusline-slowgit: process died during startup")
                cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
                return False
            text = screen.text()
            if row_at is None and stats_marker in text:
                row_at = time.time() - t0
            if b"status stale" in raw:
                stale_at = time.time() - t0
                break
        if stale_at is not None:
            print(f"FAIL ext-statusline-slowgit: stale hint at {stale_at:.1f} s; a tick reply must not wait on a slow git")
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        if row_at is None:
            print("FAIL ext-statusline-slowgit: the status row never showed")
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        if row_at > 4.0:
            print(f"FAIL ext-statusline-slowgit: the row took {row_at:.1f} s; the first tick reply must stay fast")
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        # The vim modal composer starts in insert mode: the quit gate
        # needs the insert exit key first.
        os.write(master, b"\x1b")
        pump(master, 0.3, screen)
        os.write(master, b"q")
        pump(master, 0.4, screen)
        os.write(master, b"q")
        deadline = time.time() + 4.0
        while time.time() < deadline and alive(pid):
            pump(master, 0.2, screen)
        if alive(pid):
            os.kill(pid, signal.SIGKILL)
        reap(pid)
        orphans = settled_orphans(EXTS_ROOT + "/ui_extensions")
        if orphans:
            print(f"FAIL ext-statusline-slowgit: orphan layer processes: {orphans}")
            for p in orphans:
                try:
                    os.kill(p, signal.SIGKILL)
                except OSError:
                    pass
            return False
        print(f"OK ext-statusline-slowgit: row at {row_at:.1f} s, no stale hint over the slow git (12 s window)")
        return True
    finally:
        try:
            os.close(master)
        except OSError:
            pass


def tool_result_only_layer(tmp):
    """A temp layer that carries only the tool_result demo ext.

    The demo ext lives outside the default ui_extensions layer in
    ui_extensions-demos/ (the built-in render owns tool_result in
    the default UX, the 2026-09-03 user report). This case wants
    the ext render, so it builds a private layer from the demo
    directory. Returns the layer directory.
    """
    layer = tmp + "/tr-layer"
    d = layer + "/tool_result"
    os.makedirs(d)
    src_dir = EXTS_ROOT + "/ui_extensions-demos/tool_result"
    for fn in ("ext.toml", "tool_result.sh"):
        with open(src_dir + "/" + fn) as f:
            data = f.read()
        with open(d + "/" + fn, "w") as f:
            f.write(data)
    os.chmod(d + "/tool_result.sh", 0o755)
    return layer


def ext_tool_result_kill():
    """Kill the tool_result renderer: the built-in render returns.

    A private layer carries only the tool_result demo ext (the
    default layer no longer registers it). The extension renders
    the seeded tool_result. SIGKILL the process group on every
    restart attempt until the 1 s / 2 s / 4 s budget is spent.
    The host then drops the cached replies, and a freshly appended
    tool_result renders through the built-in path.
    """
    tmp = tempfile.mkdtemp(prefix="tui-ext-kill-")
    tr_layer = tool_result_only_layer(tmp)
    cfg, sessions = layer_config(tmp, tr_layer, active_model="smoke-model")
    seed_session(sessions, EXT_SESSION, seed_events())
    tr_dir = tr_layer + "/tool_result"
    master, pid = spawn(EXT_SESSION, cfg)
    screen = Screen(24, 80)
    log = os.path.join(sessions, EXT_SESSION, "events.jsonl")
    try:
        # 1. The extension's render is in place.
        deadline = time.time() + 15.0
        while time.time() < deadline:
            pump(master, 0.2, screen)
            if not alive(pid):
                print("FAIL ext-tool-result-kill: process died during startup")
                cleanup_layer(pid, tr_layer)
                return False
            if "[ext] tool:call_1" in screen.text():
                break
        if "[ext] tool:call_1" not in screen.text():
            print("FAIL ext-tool-result-kill: the extension render never showed")
            print("screen was:\n" + screen.text())
            cleanup_layer(pid, tr_layer)
            return False
        # 2. Kill every restart generation until the budget is spent.
        # The layer is private to this case (a tmp dir), so the
        # live children of the case TUI are in scope: scan without
        # the orphan ppid gate.
        kill_deadline = time.time() + 10.0
        while time.time() < kill_deadline:
            for p in procs_with_cwd_under(tr_dir, any_parent=True):
                try:
                    os.kill(p, signal.SIGKILL)
                except OSError:
                    pass
            time.sleep(0.5)
        # 3. Append a new tool_result live. The host tailer sees it.
        with open(log, "a") as f:
            f.write(json.dumps({
                "v": 1, "type": "tool_call", "ts": "2026-08-27T10:05:00Z",
                "id": "call_9", "name": "bash",
                "arguments": {"command": "true"},
            }) + "\n")
            f.write(json.dumps({
                "v": 1, "type": "tool_result", "ts": "2026-08-27T10:05:00Z",
                "id": "call_9", "value": {"text": "late result"}, "is_error": False,
            }) + "\n")
        # 4. The dead hint flashes and the built-in render shows the
        # new result. The extension render must be gone.
        deadline = time.time() + 15.0
        saw_dead = False
        saw_builtin = False
        while time.time() < deadline:
            pump(master, 0.25, screen)
            text = screen.text()
            if "ext tool_result is dead" in text:
                saw_dead = True
            if "late result" in text and "[ext] tool:call_9" not in text:
                saw_builtin = True
            if saw_dead and saw_builtin:
                break
        if not saw_dead:
            print("FAIL ext-tool-result-kill: the dead hint never flashed")
            print("screen was:\n" + screen.text())
            cleanup_layer(pid, tr_layer)
            return False
        if not saw_builtin:
            print("FAIL ext-tool-result-kill: the built-in render did not return")
            print("screen was:\n" + screen.text())
            cleanup_layer(pid, tr_layer)
            return False
        # Quit: no orphan layer process may survive.
        # The vim modal composer starts in insert mode: the quit gate
        # needs the insert exit key first.
        os.write(master, b"\x1b")
        pump(master, 0.3, screen)
        os.write(master, b"q")
        pump(master, 0.4, screen)
        os.write(master, b"q")
        deadline = time.time() + 4.0
        while time.time() < deadline and alive(pid):
            pump(master, 0.2, screen)
        if alive(pid):
            print("FAIL ext-tool-result-kill: still running after double-q (hang)")
            os.kill(pid, signal.SIGKILL)
            reap(pid)
            cleanup_layer(pid, tr_layer)
            return False
        reap(pid)
        orphans = settled_orphans(tr_layer)
        if orphans:
            print(f"FAIL ext-tool-result-kill: orphan layer processes: {orphans}")
            for p in orphans:
                try:
                    os.kill(p, signal.SIGKILL)
                except OSError:
                    pass
            return False
        print("OK ext-tool-result-kill: ext render shown, kill -> dead hint, built-in render returns")
        return True
    finally:
        try:
            os.close(master)
        except OSError:
            pass


def ext_mermaid():
    """Host span extraction plus the reference mermaid transform.

    A valid `fence:mermaid` block in an assistant message is
    replaced by the extension's Unicode art (the raw fence is gone).
    A broken fence keeps the raw fence: the extension stays silent
    on unparseable source, and the per-op G5 fallback shows it.
    """
    tmp = tempfile.mkdtemp(prefix="tui-ext-mermaid-")
    cfg, sessions = layer_config(tmp, EXTS_ROOT + "/ui_extensions", active_model="smoke-model")
    ts = "2026-08-27T11:00:00Z"
    events = [
        {"v": 1, "type": "user_message", "ts": ts, "content": "draw the flow"},
        {
            "v": 1, "type": "assistant_message", "ts": ts,
            "content": "Here is the flow:\n\n```mermaid\ngraph TD\n  A-->B\n```\n",
            "tool_calls": [], "stop_reason": "stop",
        },
        {
            "v": 1, "type": "assistant_message", "ts": ts,
            "content": "Broken one:\n\n```mermaid\nthis is %% not a diagram\n```\n",
            "tool_calls": [], "stop_reason": "stop",
        },
    ]
    seed_session(sessions, "tui-test-mmd", events)
    master, pid = spawn("tui-test-mmd", cfg)
    screen = Screen(24, 80)
    try:
        # The art marker is a node box from the box-drawing output.
        # The raw marker is the broken fence body. The source text
        # of the valid fence must be gone (the art replaced it).
        deadline = time.time() + 15.0
        markers = ["│ A │", "not a diagram"]
        seen, all_ok = wait_markers(master, pid, screen, markers, deadline)
        text = screen.text()
        if not all_ok:
            missing = [m for m in markers if m not in seen]
            print(f"FAIL ext-mermaid: markers not seen: {missing}")
            print("screen was:\n" + text)
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        if "graph TD" in text:
            print("FAIL ext-mermaid: the valid fence shows raw, the art is missing")
            print("screen was:\n" + text)
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        # Quit: no orphan layer process may survive.
        # The vim modal composer starts in insert mode: the quit gate
        # needs the insert exit key first.
        os.write(master, b"\x1b")
        pump(master, 0.3, screen)
        os.write(master, b"q")
        pump(master, 0.4, screen)
        os.write(master, b"q")
        deadline = time.time() + 4.0
        while time.time() < deadline and alive(pid):
            pump(master, 0.2, screen)
        if alive(pid):
            print("FAIL ext-mermaid: still running after double-q (hang)")
            os.kill(pid, signal.SIGKILL)
            reap(pid)
            cleanup_layer(pid, EXTS_ROOT + "/ui_extensions")
            return False
        reap(pid)
        orphans = settled_orphans(EXTS_ROOT + "/ui_extensions")
        if orphans:
            print(f"FAIL ext-mermaid: orphan layer processes: {orphans}")
            for p in orphans:
                try:
                    os.kill(p, signal.SIGKILL)
                except OSError:
                    pass
            return False
        print("OK ext-mermaid: valid fence shows the art, broken fence shows raw")
        return True
    finally:
        try:
            os.close(master)
        except OSError:
            pass


def ext_rus():
    """The Rust reference layer on a seeded session.

    The ext-rs layer loads the Rust ports of the stage 2 bash
    references (ui-extension-plan stage 4). The statusline shows
    the same powerline footer as the bash reference: git, session,
    loop state, cumulative usage from the log. The tool_result
    renderer shows its [ext] header for the seeded result. The exit
    criterion: every surface has a bash and a Rust reference.
    """
    tmp = tempfile.mkdtemp(prefix="tui-ext-rus-")
    cfg, sessions = layer_config(tmp, EXTS_ROOT + "/ext-rs", active_model="smoke-model")
    seed_session(sessions, EXT_SESSION, seed_events())
    # The powerline footer splits the model pill and the stats
    # pill, so the two markers are separate.
    stats_marker = "in:125 out:55 sum:180"
    master, pid = spawn(EXT_SESSION, cfg)
    screen = Screen(24, 80)
    try:
        deadline = time.time() + 15.0
        seen, _ = wait_markers(
            master, pid, screen,
            ["git:none", "smoke-model", stats_marker],
            deadline,
        )
        if not alive(pid):
            print("FAIL ext-rus: process died during startup")
            cleanup_layer(pid, EXTS_ROOT + "/ext-rs")
            return False
        missing = [m for m in ["git:none", "smoke-model", stats_marker] if m not in seen]
        if missing:
            print(f"FAIL ext-rus: markers not seen: {missing}")
            print("screen was:\n" + screen.text())
            cleanup_layer(pid, EXTS_ROOT + "/ext-rs")
            return False
        # The Rust notify reference rings once: the start resend is
        # a burst, so the bell flushes when the stream goes quiet
        # past the 5 s window.
        deadline = time.time() + 7.0
        while time.time() < deadline and b"\x07" not in screen.raw:
            pump(master, 0.25, screen)
        if b"\x07" not in screen.raw:
            print("FAIL ext-rus: no terminal bell from the Rust notify reference")
            cleanup_layer(pid, EXTS_ROOT + "/ext-rs")
            return False
        # Quit: no orphan layer process may survive.
        # The vim modal composer starts in insert mode: the quit gate
        # needs the insert exit key first.
        os.write(master, b"\x1b")
        pump(master, 0.3, screen)
        os.write(master, b"q")
        pump(master, 0.4, screen)
        os.write(master, b"q")
        deadline = time.time() + 4.0
        while time.time() < deadline and alive(pid):
            pump(master, 0.2, screen)
        if alive(pid):
            print("FAIL ext-rus: still running after double-q (hang)")
            os.kill(pid, signal.SIGKILL)
            reap(pid)
            cleanup_layer(pid, EXTS_ROOT + "/ext-rs")
            return False
        reap(pid)
        orphans = settled_orphans(EXTS_ROOT + "/ext-rs")
        if orphans:
            print(f"FAIL ext-rus: orphan layer processes: {orphans}")
            for p in orphans:
                try:
                    os.kill(p, signal.SIGKILL)
                except OSError:
                    pass
            return False
        print("OK ext-rus: Rust statusline, tool_result, and notify references shown")
        return True
    finally:
        try:
            os.close(master)
        except OSError:
            pass


def goal_only_layer(tmp):
    """A temp layer that carries only the goal extension entry.

    The entry is a symlink, not a copy: the manifest resolves the
    binary by an entry-relative path (`target/debug/goal-ext`,
    docs/ui-extension.md section 6), so the entry dir must keep
    its own `target` tree. Symlinking shares the build with the
    repo tree and keeps the host's entry-relative resolution
    working.
    """
    layer = tmp + "/goal-layer"
    os.makedirs(layer)
    os.symlink(GOAL_EXT_DIR, layer + "/goal")
    return layer


def seed_goal(sessions_dir, name, goal_text="shrink the smoke pty"):
    """Seed an open goal state into a session (pointer + state file).

    Only the goal extension reads these files (`GoalState::load`,
    docs/goal-ux.md section 1.1d): the TUI has no goal-state
    coupling (docs/goal-ux.md section 1.7), which is what these
    cases prove.
    """
    d = os.path.join(sessions_dir, name)
    os.makedirs(d, exist_ok=True)
    opened = int(time.time()) - 154
    with open(d + "/goal-g-smoke0001.json", "w") as f:
        json.dump({
            "id": "g-smoke0001",
            "goal": goal_text,
            "active": True,
            "used_tokens": 12400,
            "opened_at": "t+%ds" % opened,
        }, f)
    with open(d + "/goal.json", "w") as f:
        json.dump({"current_goal": "g-smoke0001"}, f)


def ext_goal_row_installed():
    """The goal status row appears when the goal extension is installed.

    A goal-only layer + a seeded open goal: the host-reserved row
    slot (docs/ui-extension.md section 4, `row` capability) shows
    the extension-supplied goal line `⚡ "<goal>" · <elapsed> ·
    <tokens>`. Quit is clean, no orphans.
    """
    tmp = tempfile.mkdtemp(prefix="tui-goal-installed-")
    layer = goal_only_layer(tmp)
    cfg, sessions = layer_config(tmp, layer, active_model="smoke-model")
    seed_session(sessions, EXT_SESSION, seed_events())
    seed_goal(sessions, EXT_SESSION)
    master, pid = spawn(EXT_SESSION, cfg)
    screen = Screen(24, 80)
    try:
        markers = ["⚡", "shrink the smoke pty", "12.4k"]
        seen, ok = wait_markers(master, pid, screen, markers, time.time() + 15.0)
        if not ok:
            missing = [m for m in markers if m not in seen]
            print(f"FAIL ext-goal-row-installed: goal row markers not seen: {missing}")
            print("screen was:\n" + screen.text())
            cleanup_layer(pid, GOAL_EXT_DIR)
            return False
        # Double-q quit: the vim modal composer starts in insert
        # mode, so the exit key comes first.
        os.write(master, b"\x1b")
        pump(master, 0.3, screen)
        os.write(master, b"q")
        pump(master, 0.4, screen)
        os.write(master, b"q")
        deadline = time.time() + 8.0
        while time.time() < deadline and alive(pid):
            pump(master, 0.2, screen)
        if alive(pid):
            print("FAIL ext-goal-row-installed: still running after double-q (hang)")
            os.kill(pid, signal.SIGKILL)
            reap(pid)
            cleanup_layer(pid, GOAL_EXT_DIR)
            return False
        reap(pid)
        orphans = settled_orphans(GOAL_EXT_DIR)
        if orphans:
            print(f"FAIL ext-goal-row-installed: orphan layer processes: {orphans}")
            for p in orphans:
                try:
                    os.kill(p, signal.SIGKILL)
                except OSError:
                    pass
            return False
        print("OK ext-goal-row-installed: goal row seen, clean quit, no orphans")
        return True
    finally:
        try:
            os.close(master)
        except OSError:
            pass


def ext_goal_row_bare():
    """No goal row without the goal extension, even with goal state on disk.

    An empty `[ext] dir` (zero extensions) + the same seeded open
    goal: the bare TUI has no goal-state coupling, so the
    host-reserved row slot collapses to zero rows. Prove "⚡" never
    appears and the TUI still quits cleanly.
    """
    tmp = tempfile.mkdtemp(prefix="tui-goal-bare-")
    empty = tmp + "/bare-layer"
    os.makedirs(empty)
    cfg, sessions = layer_config(tmp, empty)
    seed_session(sessions, EXT_SESSION, seed_events())
    seed_goal(sessions, EXT_SESSION)
    master, pid = spawn(EXT_SESSION, cfg)
    screen = Screen(24, 80)
    try:
        # Pump well past several tick intervals: with no row owner
        # the slot stays empty.
        pump(master, 6.0, screen)
        if not alive(pid):
            print("FAIL ext-goal-row-bare: process died during startup")
            return False
        text = screen.text()
        if "⚡" in text:
            print("FAIL ext-goal-row-bare: goal row without the goal extension")
            print("screen was:\n" + text)
            os.kill(pid, signal.SIGKILL)
            reap(pid)
            return False
        os.write(master, b"\x1b")
        pump(master, 0.3, screen)
        os.write(master, b"q")
        pump(master, 0.4, screen)
        os.write(master, b"q")
        deadline = time.time() + 4.0
        while time.time() < deadline and alive(pid):
            pump(master, 0.2, screen)
        if alive(pid):
            print("FAIL ext-goal-row-bare: still running after double-q (hang)")
            os.kill(pid, signal.SIGKILL)
            reap(pid)
            return False
        reap(pid)
        print("OK ext-goal-row-bare: no goal row without the goal extension")
        return True
    finally:
        try:
            os.close(master)
        except OSError:
            pass


def purge_strays():
    """Kill pre-existing stray extension processes before the suite.

    An aborted run or a killed terminal leaks extension children
    (each ext does its own setsid, so they survive their TUI's
    death, docs/ui-extension.md section 7). Their cwd sits under
    a layer dir, so a later settled_orphans check flags them and
    the case fails on stale state. Purge them at suite start.
    """
    prefixes = [
        EXTS_ROOT + "/ui_extensions",
        EXTS_ROOT + "/ext-rs",
    ]
    killed = []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            cwd = os.readlink(f"/proc/{d}/cwd")
        except OSError:
            continue
        # Skip extensions of a live TUI: a stray is reparented to
        # init, a live TUI's children keep their parent.
        if proc_ppid(d) != 1:
            continue
        for p in prefixes:
            if cwd.startswith(p + "/"):
                try:
                    os.kill(int(d), signal.SIGKILL)
                    killed.append(int(d))
                except OSError:
                    pass
                break
    if killed:
        time.sleep(0.5)
        print(f"purged {len(killed)} stray ext process(es): {killed}")


def main():
    setup_ext_bins()
    purge_strays()
    ok = True
    ok &= case("baseline-double-q", 0)
    ok &= case("burst-300-then-double-q", 300)
    ok &= case("burst-1000-then-double-q", 1000)
    ok &= scroll_burst_reaches_head()
    ok &= ext_stub_alive()
    ok &= ext_frame_commandline()
    ok &= ext_dying_hint()
    ok &= ext_badjsonl()
    ok &= ext_append_reject()
    ok &= ext_statusline_real()
    ok &= ext_statusline_repo()
    ok &= ext_statusline_slowgit()
    ok &= ext_tool_result_kill()
    ok &= ext_mermaid()
    ok &= ext_rus()
    ok &= ext_goal_row_installed()
    ok &= ext_goal_row_bare()
    if not ok:
        sys.exit(1)
    print("ALL SMOKE CASES PASSED")


if __name__ == "__main__":
    main()
