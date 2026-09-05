#!/usr/bin/env python3
"""Probe 4: is the frame fixture process alive under the TUI?"""
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


def main():
    tmp = tempfile.mkdtemp(prefix="frame-live-")
    ext_layer = tmp + "/layer"
    os.makedirs(ext_layer, exist_ok=True)
    os.system(f"cp -r {REPO}/scripts/ext-fixture/frame/* {ext_layer}/")
    sessions = tmp + "/sessions"
    os.makedirs(sessions)
    cfg = tmp + "/config.toml"
    with open(cfg, "w") as f:
        f.write(f'[paths]\nsessions_root = "{sessions}"\n\n[ext]\ndir = "{ext_layer}"\n')

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

    time.sleep(2.0)
    found = []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/cmdline", "rb") as f:
                cmd = f.read().replace(b"\0", b" ").decode(errors="replace").strip()
        except OSError:
            continue
        if "frame.sh" in cmd:
            found.append((d, cmd))
    print("frame.sh processes:", found)
    os.kill(pid, 15)
    os.waitpid(pid, 0)
    os.close(master)


if __name__ == "__main__":
    main()
