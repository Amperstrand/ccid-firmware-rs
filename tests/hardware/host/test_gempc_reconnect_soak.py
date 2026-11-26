#!/usr/bin/env python3
"""#444-class reconnect soak vs gempc_sim — committed, bounded form.

The full 200-cycle + 2-restart soak ran ad-hoc (200/200 OK, evidence on
issue #444, comment 7085582882). This banks the class as a fast
regression: N connect/transmit/disconnect cycles + one mid-soak pcscd
restart, asserting every cycle completes and latency never degrades
(the deafness signature = failures or escalating ms).
"""
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

pytest.importorskip("smartcard")
if not (shutil.which("/usr/sbin/pcscd") or shutil.which("pcscd")):
    pytest.skip("pcscd not installed", allow_module_level=True)

HERE = Path(__file__).resolve().parent
SOCK = "/run/pcscd-lane-gempc-soak.sock"
CYCLES = 30
RESTART_AT = CYCLES // 2

CLIENT = """
import time
from smartcard.System import readers
t0 = time.time()
rs = [x for x in readers() if "GemPC Twin SIM" in str(x)]
assert rs, "no reader"
conn = rs[0].createConnection()
conn.connect()
conn.transmit(list(bytes.fromhex("00A4040007D276000085010100")))
conn.disconnect()
print(int((time.time() - t0) * 1000))
"""


@pytest.mark.timeout(240)
def test_reconnect_soak_with_pcscd_restart(tmp_path):
    sim = act = None
    latencies = []
    try:
        sim = subprocess.Popen(
            [sys.executable, str(HERE / "gempc_sim.py"),
             "--respond", "9000"],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        port = sim.stdout.readline().decode().strip()
        conf = tmp_path / "conf"
        conf.mkdir()
        (conf / "r").write_text(
            f'FRIENDLYNAME "GemPC Twin SIM (gempc_sim)"\n'
            f"DEVICENAME {port}:GemPCTwin\n"
            f"LIBPATH /usr/lib/pcsc/drivers/serial/libccidtwin.so\n")

        def start():
            try:
                os.unlink(SOCK)
            except FileNotFoundError:
                pass
            return subprocess.Popen(
                ["systemd-socket-activate", "-l", SOCK, "--",
                 "/usr/sbin/pcscd", "--foreground", "-c", str(conf)],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        act = start()
        time.sleep(1.5)
        env = dict(os.environ, PCSCLITE_CSOCK_NAME=SOCK)

        for i in range(CYCLES):
            if i == RESTART_AT:
                act.terminate()
                act.wait(timeout=10)
                act = start()
                time.sleep(1.5)
            r = subprocess.run([sys.executable, "-c", CLIENT], env=env,
                               capture_output=True, text=True, timeout=60)
            assert r.returncode == 0, (
                f"cycle {i} failed: {r.stderr[-200:]!r}")
            latencies.append(int(r.stdout.strip().splitlines()[-1]))

        # No failures + no degradation: the post-restart band may be
        # briefly higher (pcscd cold start) but must settle; assert the
        # LAST quarter is not systematically slower than the first.
        first = sum(latencies[: CYCLES // 4]) / (CYCLES // 4)
        last = sum(latencies[-(CYCLES // 4):]) / (CYCLES // 4)
        assert last < first * 2 + 200, (
            f"latency degraded: first~{first:.0f}ms last~{last:.0f}ms "
            f"all={latencies}")
    finally:
        for p in (sim, act):
            if p:
                try:
                    p.terminate()
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
