#!/usr/bin/env python3
"""Probe 5: run the TUI on a pty with the debug frame layer, then
dump the fixture debug log to see the ops and replies."""
import os
import pty
import struct
import sys
import tempfile
import termios
import fcntl
import time

BIN = sys.argv[1]
REPO = sys.argv[2]
LAYER = "/tmp/frame-debug-layer"


def main():
    tmp = tempfile.mkdtemp(prefix="frame-debug-")
    sessions = tmp + "/sessions"
    os.makedirs(sessions)
    cfg = tmp + "/config.toml"
    with open(cfg, "w") as f:
        f.write(f'[paths]\nsessions_root = "{sessions}"\n\n[ext]\ndir = "{LAYER}"\n')

    log = os.environ["HOME"] + "/frame-fixture-debug.log"
    if os.path.exists(log):
        os.unlink(log)

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
        os.execv(BIN, [BIN, "tui-test-frame", "--config", cfg])
    os.close(slave)
    time.sleep(3.0)
    import signal

    os.kill(pid, signal.SIGTERM)
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass
    os.close(master)
    if os.path.exists(log):
        print(open(log).read()[:4000])
    else:
        print("NO LOG: the fixture never started")


if __name__ == "__main__":
    main()
