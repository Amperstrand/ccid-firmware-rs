#!/usr/bin/env python3
"""#547 rf-dark build plumbing test — the board entry resolves.

The rfdark main itself compiles only on esp targets (diag-main
doctrine: first compile at the bench), but the build.sh plumbing is
host-testable: --dry-run resolves the board entry end-to-end and this
pins the contract (toolchain, triple, features, chip, artifact name).
If the entry rots (renamed feature, wrong triple), this fails before
anyone burns a bench session on it.
"""
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
BUILD_SH = HERE.parents[2] / "firmware" / "esp32-ccid" / "build.sh"


def test_rfdark_board_entry_resolves():
    r = subprocess.run(
        [str(BUILD_SH), "pn7160-rfdark", "--dry-run"],
        capture_output=True, text=True, timeout=120)
    out = r.stdout + r.stderr
    assert r.returncode == 0, out[-400:]
    assert "board=pn7160-rfdark" in out
    assert "target=riscv32imc-esp-espidf" in out
    assert "chip=esp32c3" in out
    # the resolved feature set — the whole wiring of the dark main
    assert "--features pn7160-rfdark" in out
    # artifact naming (the OTA/serial push path depends on it)
    assert "esp32-ccid-pn7160-rfdark.bin" in out
