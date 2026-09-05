#!/usr/bin/env python3
"""Run only the ext-frame-commandline smoke case (fast iteration)."""
import importlib.util
import sys

BIN = sys.argv[1]
REPO = sys.argv[2]

spec = importlib.util.spec_from_file_location(
    "smoke", "scripts/tui-pty-smoke.py"
)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
m.BIN = BIN
m.REPO = REPO
ok = m.ext_frame_commandline()
sys.exit(0 if ok else 1)
