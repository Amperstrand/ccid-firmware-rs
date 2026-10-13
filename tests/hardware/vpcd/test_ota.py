"""OTA reflash round-trip (opt-in — NEVER runs by default).

Wire protocol and rollback hardening: firmware/esp32-ccid/src/ota.rs.
Host procedure: runbook "Firmware build + flash" (ota_push.py).

Flashing is destructive to the running firmware, so this test is skipped
unless NUCULA_OTA_BIN explicitly points at a .bin image. The expected
reflash flow for a bench iteration is: rebuild wifi-vpcd, point
NUCULA_OTA_BIN at the fresh esp32-ccid-wifi-vpcd.bin, run this module.

Recovery knowledge encoded here (bench 2026-10-13):
- after the OTA reboot, pcscd may still hold the OLD half-open vpcd
  session; its single-accept driver then never serves the board's new
  outbound connection. The runbook pcscd restart sequence clears it
  (recovery fixture, only on this failure mode).
- wait windows are generous (board reboot + WiFi + vpcd reconnect
  normally completes in ~10 s; 60 s budget per the bench brief).
"""

import os
import socket
import subprocess
import sys
import time
from pathlib import Path

import pytest

from .conftest import OTA_PORT, real_connect

OTA_PUSH = Path(__file__).with_name("ota_push.py")


@pytest.fixture(scope="module")
def ota_bin():
    path = os.environ.get("NUCULA_OTA_BIN", "")
    if not path:
        pytest.skip("NUCULA_OTA_BIN not set — OTA reflash disabled by default")
    if not path.endswith(".bin") or not os.path.isfile(path):
        pytest.fail(f"NUCULA_OTA_BIN={path!r} is not a readable .bin image")
    return path


def _ota_port_up(board_ip):
    try:
        with socket.create_connection((board_ip, OTA_PORT), timeout=2):
            return True
    except OSError:
        return False


def _board_back(board_ip, vpcd_port, vpcd_estab):
    """Board is 'back' when the fresh OTA server answers AND its vpcd
    listener session is re-established (runbook "Architecture": the board
    connects out; ESTAB proves WiFi + app + vpcd thread are all up)."""
    return _ota_port_up(board_ip) and vpcd_estab()


def test_ota_reflash_and_board_returns(ota_bin, board_ip, vpcd_port,
                                       vpcd_estab, restart_pcscd):
    """Push the image over :3232, then wait <=60s for the board to serve
    again; one runbook pcscd restart is allowed to clear a stale half-open
    vpcd session before giving up."""
    p = subprocess.run([sys.executable, str(OTA_PUSH), board_ip, ota_bin],
                       capture_output=True, text=True, timeout=300)
    assert p.returncode in (0, 2), (p.returncode, p.stdout, p.stderr)
    # rc 2 = image accepted but no ESTAB seen yet — keep polling below.

    deadline = time.time() + 60
    while time.time() < deadline and not _board_back(board_ip, vpcd_port, vpcd_estab):
        time.sleep(3)

    if not _board_back(board_ip, vpcd_port, vpcd_estab):
        # Stale half-open ESTAB from the pre-reboot session can satisfy
        # vpcd_estab() while pcscd still serves the dead socket; or the
        # board's new connection waits on the driver's single accept.
        # Runbook restart sequence (conftest fixture) re-accepts cleanly.
        restart_pcscd()
        deadline = time.time() + 30
        while time.time() < deadline and not _board_back(board_ip, vpcd_port, vpcd_estab):
            time.sleep(3)

    assert _ota_port_up(board_ip), (
        f"OTA server on {board_ip}:{OTA_PORT} did not come back after reflash")
    assert vpcd_estab(), (
        "no ESTAB session on the vpcd listener after reflash — board did not "
        "reconnect (check netlog/serial; bench brief allows 60s)")

    # Best-effort card check (real connect, runbook rule 2): logged, not
    # asserted — card coupling after a reboot is a bench variable
    # (runbook "Power") and is covered by test_link/test_smartpgp suites.
    try:
        atr = real_connect(next(n for n in ("Nucula VPCD 00 00", "Nucula VPCD 00 01")
                                if _slot_exists(n)))
        print(f"\npost-reflash card probe: ATR {atr.hex().upper() or '(empty reader)'}")
    except Exception as e:
        print(f"\npost-reflash card probe skipped: {type(e).__name__}: {e}")


def _slot_exists(name):
    from smartcard.System import readers
    return any(str(r) == name for r in readers())
