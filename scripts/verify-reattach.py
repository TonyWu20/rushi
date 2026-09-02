#!/usr/bin/env python3
r"""verify-reattach: the FT-003 loop-reattach checks on a live loop.

An application (docs/skill-remapped-to-os-apps.md sections 1-4): a
project-specific, short-lived command on the agent-visible path
(`scripts/`), needed only while the TUI loop-reattach behavior is
under development (`docs/tui.md`, FT-003). It is not in the base
distribution. Discovery is on demand: `ls scripts/`, then this
`--help`. There is no SKILL.md: the interface below is the
documentation.

The screen checks reuse the Screen grid from `scripts/tui-pty-smoke.py`
(the shared component, composed rather than re-implemented, as
`tui-capture.py` does).

WHAT IT DOES
  Opens the TUI on a session whose loop outlived an earlier TUI and
  checks:
    1. The [running] bit shows on the restarted TUI (not [idle]).
    2. Ctrl+R blocks on the persistent probe (no duplicate start).
    3. The external loop survives the TUI quit.
    4. A loop_reattach record lands in the session trace log.
  The script never presses Ctrl+C: the checked loop must stay alive.
  Exit 0 when every check passes (or the loop is not live, SKIP),
  1 on FAIL.

USAGE
  verify-reattach.py [--repo PATH] [--bin PATH] [--session NAME]

EXAMPLES
  # the default: this repo, target/debug/tui, the better-ui session:
  verify-reattach.py
  # a session whose loop was just started with turn.sh:
  verify-reattach.py --session better-ui
"""
import argparse
import importlib.util
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)


def env_or(name, default):
    """The old env override (REPO, SESSION) stays available."""
    return os.environ.get(name, default)


def load_smoke():
    """Import the pty helpers from tui-pty-smoke.py.

    The smoke script is a script, not a module (the dash in the name),
    and reads sys.argv[1:2] at import time for its binary and repo
    paths. Those are irrelevant to the helpers used here, but would
    raise on a short argv. The argv is padded for the import and
    restored afterwards.
    """
    smoke_path = os.path.join(HERE, "tui-pty-smoke.py")
    spec = importlib.util.spec_from_file_location("tui_pty_smoke", smoke_path)
    mod = importlib.util.module_from_spec(spec)
    saved_argv = sys.argv
    sys.argv = [saved_argv[0], "tui-pty-smoke", REPO]
    try:
        spec.loader.exec_module(mod)
    finally:
        sys.argv = saved_argv
    return mod


def loop_alive(pid):
    return os.path.exists("/proc/%d" % pid)


def main():
    ap = argparse.ArgumentParser(
        description="Verify the FT-003 reattach on a session whose loop "
                    "outlived an earlier TUI.")
    ap.add_argument("--repo", default=env_or("REPO", REPO),
                    help="the repo root (env REPO; default: the parent of scripts/)")
    ap.add_argument("--bin", default=None,
                    help="the tui binary (default: $repo/target/debug/tui)")
    ap.add_argument("--session", default=env_or("SESSION", "better-ui"),
                    help="the session with a live external loop (env SESSION)")
    args = ap.parse_args()
    if args.bin is None:
        args.bin = os.path.join(args.repo, "target", "debug", "tui")

    smoke = load_smoke()

    pid_file = os.path.join(args.repo, "sessions", args.session, "loop.pid")
    if not os.path.exists(pid_file):
        print("SKIP: no loop.pid for session %s (no external loop started)" % args.session)
        return 0
    with open(pid_file) as f:
        loop_pid = int(f.read().strip())
    if not loop_alive(loop_pid):
        print("SKIP: external loop %d is not live" % loop_pid)
        return 0

    master, pid = smoke.spawn(args.session)
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
    trace = os.path.join(args.repo, "sessions", args.session, "tui-trace.jsonl")
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
