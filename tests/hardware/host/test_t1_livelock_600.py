#!/usr/bin/env python3
"""#600 regression lock: pcscd T=1 LIVES-LOCKS against chain-abort.

Characterization test (documents upstream behavior, found via
gempc_sim's adversary): when the card answers an I-block request with
a chain M-block and replays it forever, pcscd's internal T=1 stack
never surfaces an error — SCardTransmit hangs indefinitely (mechanism:
reference/CCID/src/openct/proto-t1.c — the M-block path sends an
R-block asking for the next piece WITHOUT decrementing `retries`; a
sequence-correct infinite chain has no ceiling).

This test pins the signature BOUNDED: the client runs under a hard
harness timeout and we assert the transmit did NOT complete within it
— if upstream ever fixes the liveness edge (or our endpoint contract
changes), this flips and tells us.

Host-only (needs pcscd + libccidtwin + pyscard — the bench/virtual-card
CI class); skipped anywhere those are absent.
"""
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

pytest.importorskip("smartcard")
if not shutil.which("/usr/sbin/pcscd") and not shutil.which("pcscd"):
    pytest.skip("pcscd not installed", allow_module_level=True)

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
SOCK = "/run/pcscd-lane-gempc-600.sock"
CLIENT_TIMEOUT_S = 8  # far below the observed >120 s hang; the point
                      # is that NO fast error path exists


@pytest.mark.timeout(120)
def test_chain_abort_livelock_is_bounded_by_harness_not_pcscd(tmp_path):
    sim = subprocess.Popen(
        [sys.executable, str(HERE / "gempc_sim.py"),
         "--t1-adversary", "chain-abort"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    act = None
    try:
        port = sim.stdout.readline().decode().strip()
        assert port.startswith("/dev/pts/"), port
        conf = tmp_path / "conf"
        conf.mkdir()
        (conf / "r").write_text(
            f'FRIENDLYNAME "GemPC Twin SIM (gempc_sim)"\n'
            f"DEVICENAME {port}:GemPCTwin\n"
            f"LIBPATH /usr/lib/pcsc/drivers/serial/libccidtwin.so\n")
        try:
            os.unlink(SOCK)
        except FileNotFoundError:
            pass
        act = subprocess.Popen(
            ["systemd-socket-activate", "-l", SOCK, "--",
             "/usr/sbin/pcscd", "--foreground", "-c", str(conf)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1.5)
        env = dict(os.environ, PCSCLITE_CSOCK_NAME=SOCK)
        r = subprocess.run(
            [sys.executable, "-c", """
from smartcard.System import readers
rs = readers()
assert rs, "no readers"
conn = rs[0].createConnection()
conn.connect()
d, s1, s2 = conn.transmit(list(bytes.fromhex("00A4040008A000000151000000")))
print(f"COMPLETED {s1:02X}{s2:02X}")
"""],
            env=env, capture_output=True, text=True,
            timeout=CLIENT_TIMEOUT_S)
        # If we get here the transmit COMPLETED — upstream behavior
        # changed: re-verify the mechanism and update #600.
        assert False, (
            "transmit completed under chain-abort (stdout: "
            f"{r.stdout.strip()!r}) — the livelock signature is GONE; "
            "re-check proto-t1.c and update issue #600")
    except subprocess.TimeoutExpired:
        pass  # the documented signature: hang, bounded only by us
    finally:
        for p in (sim, act):
            if p:
                p.terminate()
        time.sleep(0.3)
        for p in (sim, act):
            if p:
                p.kill()
        try:
            os.unlink(SOCK)
        except FileNotFoundError:
            pass
