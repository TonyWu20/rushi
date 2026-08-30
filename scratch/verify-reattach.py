#!/usr/bin/env python3
"""Verify the FT-003 reattach on a real session.

Opens the TUI on a session whose loop outlived an earlier TUI.
Checks:
1. The [running] bit shows on a restarted TUI (not [idle]).
2. Ctrl+R blocks on the persistent probe (no duplicate start).
3. The external loop survives the TUI quit.
4. A loop_reattach record lands in the session trace log.

The script never presses Ctrl+C: the checked loop must stay alive.
"""
import importlib.util
import json
import os
import sys
import time

REPO = os.environ.get("REPO", "/home/tony/programming/rust-unix-harness")
BIN = os.path.join(REPO, "target", "debug", "tui")
SESSION = os.environ.get("SESSION", "better-ui")

sys.argv = [__file__, BIN, REPO]
spec = importlib.util.spec_from_file_location(
    "smoke", REPO + "/scripts/tui-pty-smoke.py"
)
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


def loop_alive(pid):
    return os.path.exists("/proc/%d" % pid)


def main():
    pid_file = os.path.join(REPO, "sessions", SESSION, "loop.pid")
    with open(pid_file) as f:
        loop_pid = int(f.read().strip())
    if not loop_alive(loop_pid):
        print("SKIP: external loop %d is not live" % loop_pid)
        return 0

    master, pid = smoke.spawn(SESSION)
    screen = smoke.Screen(24, 80)
    ok = True
    try:
        # 1. The [running] bit must show for the live external loop.
        deadline = time.time() + 10.0
        saw_running = False
        while time.time() < deadline:
            smoke.pump(master, 0.2, screen)
            if not smoke.alive(pid):
                print("FAIL: TUI died at startup")
                return 1
            if "[running]" in screen.text():
                saw_running = True
                break
        if not saw_running:
            print("FAIL: [running] bit never showed on the restarted TUI")
            print(screen.text())
            return 1
        print("OK running-bit: restarted TUI shows the live external loop")

        # 2. Ctrl+R must block on the probe. The flash says the loop
        # is already active, and no second loop starts.
        os.write(master, b"\x12")
        deadline = time.time() + 5.0
        blocked = False
        while time.time() < deadline:
            smoke.pump(master, 0.2, screen)
            if "loop already active" in screen.text():
                blocked = True
                break
        if not blocked:
            print("FAIL: Ctrl+R did not block on the probe")
            print(screen.text())
            return 1
        print("OK duplicate-start-block: Ctrl+R refused while the loop is live")
    finally:
        os.write(master, b"q")
        smoke.pump(master, 0.4, screen)
        os.write(master, b"q")
        deadline = time.time() + 4.0
        while time.time() < deadline and smoke.alive(pid):
            smoke.pump(master, 0.2, screen)
        if smoke.alive(pid):
            os.kill(pid, 9)
        smoke.reap(pid)
        os.close(master)

    if not loop_alive(loop_pid):
        print("FAIL: the external loop did not survive the TUI quit")
        return 1
    print("OK loop-survives: the external loop stayed alive")

    # 4. The trace log must hold a loop_reattach record.
    trace = os.path.join(REPO, "sessions", SESSION, "tui-trace.jsonl")
    kinds = []
    if os.path.exists(trace):
        with open(trace) as f:
            for line in f:
                try:
                    kinds.append(json.loads(line)["kind"])
                except Exception:
                    pass
    if "loop_reattach" not in kinds:
        print("FAIL: no loop_reattach record in the trace log")
        print("recent kinds: %s" % kinds[-5:])
        return 1
    print("OK trace: loop_reattach record written")
    print("ALL REATTACH CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
