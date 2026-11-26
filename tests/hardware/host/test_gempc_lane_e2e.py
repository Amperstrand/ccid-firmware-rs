#!/usr/bin/env python3
"""gempc_sim lane-pcscd END-TO-END test — the committed form of the
proof that lived only in /tmp scripts (wiped twice by reboots).

Banks the positive path (PR #596's VERDICT: PASS recipe): sim on a pty
-> lane pcscd via systemd-socket-activate (the AGENTS doctrine
mechanism) -> pyscard enumerates the reader, SCardConnect through
pcscd's internal T=1, transmits round-trip. Skips where pcscd /
libccidtwin / pyscard are absent (CI minimal envs).
"""
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

pytest.importorskip("smartcard")
if not (shutil.which("/usr/sbin/pcscd") or shutil.which("pcscd")):
    pytest.skip("pcscd not installed", allow_module_level=True)
if not Path("/usr/lib/pcsc/drivers/serial/libccidtwin.so").exists():
    pytest.skip("libccidtwin not installed", allow_module_level=True)

HERE = Path(__file__).resolve().parent
SOCK = "/run/pcscd-lane-gempc-e2e.sock"
SELECT = "00A4040007D276000085010100"


@pytest.mark.timeout(90)
def test_lane_pcscd_end_to_end(tmp_path):
    sim = act = None
    try:
        sim = subprocess.Popen(
            [sys.executable, str(HERE / "gempc_sim.py"),
             "--respond", "9000"],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
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
            [sys.executable, "-c", f"""
from smartcard.System import readers
rs = [x for x in readers() if "GemPC Twin SIM" in str(x)]
assert rs, f"no sim reader: {{[str(x) for x in readers()]}}"
conn = rs[0].createConnection()
conn.connect()
atr = bytes(conn.getATR())
assert atr.hex().upper() == "3B858131FE458073C8211084", atr.hex()
d, s1, s2 = conn.transmit(list(bytes.fromhex("{SELECT}")))
assert (s1, s2) == (0x90, 0x00), f"SW={{s1:02X}}{{s2:02X}}"
d, s1, s2 = conn.transmit(list(bytes.fromhex("80CA9F7F00")))
assert (s1, s2) == (0x90, 0x00)
print("E2E-OK")
"""],
            env=env, capture_output=True, text=True, timeout=60)
        assert "E2E-OK" in r.stdout, (
            f"stdout={r.stdout[-300:]!r} stderr={r.stderr[-300:]!r}")
    finally:
        for p in (sim, act):
            if p:
                try:
                    p.send_signal(signal.SIGTERM)
                except ProcessLookupError:
                    pass
        time.sleep(0.3)
        for p in (sim, act):
            if p:
                p.kill()
        try:
            os.unlink(SOCK)
        except FileNotFoundError:
            pass
