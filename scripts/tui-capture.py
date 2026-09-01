#!/usr/bin/env python3
r"""tui-capture: what the tui renders, and what it emitted.

The first *application* in this harness
(docs/skill-remapped-to-os-apps.md): a short-lived, self-documenting
command on the agent-visible path (`scripts/`), needed only while the
TUI is under development. It is not in the base distribution; it is
installed for this project and discovered on demand -- `ls scripts/`,
then this `--help`. There is no SKILL.md: the interface below *is*
the documentation, fetched when needed and landing in the transcript
tail, never the prompt prefix.

WHAT IT DOES
  Runs the `tui` binary against a session under a pty at a chosen size,
  replays the frame stream into a terminal grid (ratatui diffs frames:
  it writes only the cells that changed, so the replayed grid is the
  real display), and reports
    - every SGR sequence in the raw stream (counts, most common first),
      so "the border turned yellow" is a substring search over the
      emitted codes, not a human eyeball;
    - the replayed screen text;
    - each --expect / --expect-absent substring, checked against the
      screen text and the literal SGR sequences (\x1b rendered as \e).
  Exit 0 when every --expect matches and no --expect-absent is present.

PREPARE A SESSION
  A session with content to render must be seeded first: append its
  events with the `log` binary (bin/log). The capture itself writes
  nothing but the optional --raw-out dump.

USAGE
  tui-capture.py SESSION [--cols N] [--rows N] [--seconds N]
                     [--expect SUB]... [--expect-absent SUB]...
                     [--raw-out PATH] [--config PATH] [--bin PATH]
                     [--json]

EXAMPLES
  # the yellow input-border family (docs/tui-thinking-level-input-box.md):
  tui-capture.py my-session --expect "38;5;3"
  # dark gray, not yellow:
  tui-capture.py my-session --expect "38;5;8" --expect-absent "38;5;3"
  # plain baseline render:
  tui-capture.py my-session
  # machine-readable result, for a TUI or a test runner:
  tui-capture.py my-session --expect "38;5;3" --json

The raw stream is pipeable: capture --raw-out /tmp/raw.bin and grep it.
"""
import argparse
import fcntl
import importlib.util
import json
import os
import pty
import re
import select
import signal
import struct
import sys
import termios
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)


def load_screen_class():
    """Import the Screen terminal replayer from tui-pty-smoke.py.

    The Screen grid is a shared component: tui-capture composes it
    rather than re-implementing it (the Unix choice). The smoke script
    is a script, not a module (the dash in the name), and reads
    sys.argv[1:2] at import time for its binary and repo paths, which
    are irrelevant to the Screen class but would raise on a short
    argv. The argv is padded for the import and restored afterwards.
    """
    smoke = os.path.join(HERE, "tui-pty-smoke.py")
    spec = importlib.util.spec_from_file_location("tui_pty_smoke", smoke)
    mod = importlib.util.module_from_spec(spec)
    saved_argv = sys.argv
    sys.argv = [saved_argv[0], "tui-capture", REPO]
    try:
        spec.loader.exec_module(mod)
    finally:
        sys.argv = saved_argv
    return mod.Screen


def spawn(bin_path, session, config, cols, rows, prepend_path=None):
    """Fork the tui binary against a session under a pty at cols x rows.

    Returns (master_fd, pid). The child is detached, its stdio bound to
    the slave, and optionally given a PATH prefix (for the Rust ext
    binaries the host expects on the search path).
    """
    master, slave = pty.openpty()
    winsz = struct.pack("HHHH", rows, cols, 0, 0)
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
        if prepend_path:
            os.environ["PATH"] = prepend_path + os.pathsep + os.environ.get("PATH", "")
        os.execv(bin_path, [bin_path, session, "--config", config])
    os.close(slave)
    return master, pid


def pump(master, seconds, screen):
    """Drain pty output for `seconds`, feeding each chunk to the grid.

    A real terminal drains output all the time, so the pump keeps the
    pty queue from filling and stalling the TUI's draw call.
    """
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([master], [], [], 0.1)
        if r:
            try:
                screen.feed(os.read(master, 65536))
            except OSError:
                break


def sgr_sequences(raw):
    """Every SGR sequence in the stream, escape rendered as `\\e`."""
    out = []
    for m in re.finditer(rb"\x1b\[([0-9;]*)m", raw):
        out.append("\\e[" + m.group(1).decode() + "m")
    return out


def sgr_counts(raw):
    """SGR sequence -> count, ordered by (-count, sequence) so the
    output is deterministic across runs."""
    counts = {}
    for s in sgr_sequences(raw):
        counts[s] = counts.get(s, 0) + 1
    return dict(sorted(counts.items(), key=lambda kv: (-kv[1], kv[0])))


def main():
    ap = argparse.ArgumentParser(
        prog="tui-capture",
        description="Capture what the tui binary renders under a pty, "
                    "and assert on the emitted SGR sequences.",
        epilog=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("session", help="the TUI session name under sessions/")
    ap.add_argument("--config", default=os.path.join(REPO, "config.toml"))
    ap.add_argument("--cols", type=int, default=150)
    ap.add_argument("--rows", type=int, default=40)
    ap.add_argument("--seconds", type=int, default=4,
                    help="how long to watch the pty before quitting")
    ap.add_argument("--expect", action="append", default=[],
                    help="substring that must appear in the screen text or "
                         "the emitted SGR sequences (repeatable)")
    ap.add_argument("--expect-absent", action="append", default=[],
                    help="substring that must NOT appear in the screen "
                         "text or the emitted SGR sequences (repeatable)")
    ap.add_argument("--raw-out",
                    help="dump the raw pty stream to this path")
    ap.add_argument("--bin", default=os.path.join(REPO, "target/debug/tui"))
    ap.add_argument("--json", action="store_true",
                    help="print a machine-readable result object instead of "
                         "the human-readable report")
    args = ap.parse_args()

    screen_cls = load_screen_class()
    master, pid = spawn(args.bin, args.session, args.config, args.cols,
                        args.rows)
    screen = screen_cls(args.rows, args.cols)
    try:
        pump(master, args.seconds, screen)
        os.write(master, b"q")
        pump(master, 0.5, screen)
    finally:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(pid, 0)
        try:
            os.close(master)
        except OSError:
            pass
    raw = bytes(screen.raw)

    if args.raw_out:
        with open(args.raw_out, "wb") as f:
            f.write(raw)

    counts = sgr_counts(raw)
    total_sgr = sum(counts.values())
    screen_text = screen.text()

    # A pattern matches the replayed screen text or a literal SGR
    # sequence (escape rendered as \e).
    blob = screen_text + "\n" + "\n".join(counts)
    expect = {pat: (pat in blob) for pat in args.expect}
    expect_absent = {pat: (pat in blob) for pat in args.expect_absent}
    ok = all(expect.values()) and not any(expect_absent.values())

    if args.json:
        print(json.dumps({
            "session": args.session,
            "cols": args.cols,
            "rows": args.rows,
            "seconds": args.seconds,
            "raw_bytes": len(raw),
            "sgr_total": total_sgr,
            "sgr_counts": counts,
            "expect": expect,
            "expect_absent": expect_absent,
            "pass": ok,
        }))
    else:
        if args.raw_out:
            print(f"raw stream: {len(raw)} bytes -> {args.raw_out}")
        print(f"== tui-capture: session={args.session} cols={args.cols} "
              f"rows={args.rows} seconds={args.seconds}")
        print(f"-- SGR sequences ({total_sgr} total, "
              f"{len(counts)} distinct, most common first):")
        for seq, n in list(counts.items())[:30]:
            print(f"   {n:>5}  {seq}")
        print("-- screen text (replayed grid, non-blank rows):")
        for row in screen_text.splitlines():
            if row.strip():
                print(f"   | {row}")
        for pat, hit in expect.items():
            print(f"expect {pat!r}: " + ("MATCH" if hit else "MISSING"))
        for pat, present in expect_absent.items():
            print(f"expect-absent {pat!r}: "
                  + ("OK (absent)" if not present else "PRESENT"))
        print("PASS" if ok else "FAIL")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
