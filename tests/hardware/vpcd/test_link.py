"""Link-level bench checks for the vpcd-over-WiFi path.

Each test proves one channel of the reversed architecture
(docs/VPCD_WIFI_RUNBOOK.md "Architecture"):

  board --(ping)--> host
  board :3232  <-- OTA TCP server (firmware/esp32-ccid/src/ota.rs)
  board --UDP :4567 broadcast--> netlog (firmware/esp32-ccid/src/netlog.rs)
  board --TCP :35963 (outbound)--> libifdvpcd slot -> pcscd -> pyscard

All reader presence comes from REAL SCardConnect (runbook "Operational
rules" #2). Timeouts are generous: APDUs over this transport take ~2 s
(runbook "GPG").
"""

import os
import socket
import subprocess
import time

import pytest

from .conftest import OTA_PORT

#: ATR prefix of the bench SmartPGP card (contactless ISO-DEP activation).
SMARTPGP_ATR_PREFIX = "3B85"


def test_board_pingable(board_ip):
    """Board answers ICMP — basic liveness before anything else."""
    p = subprocess.run(["ping", "-c", "1", "-W", "2", board_ip],
                       capture_output=True, timeout=10)
    assert p.returncode == 0, (
        f"board {board_ip} did not answer ping\n"
        f"{p.stdout.decode(errors='replace')}\n{p.stderr.decode(errors='replace')}")


def test_ota_port_connectable(board_ip):
    """OTA TCP server accepts connections on :3232 (src/ota.rs listener)."""
    with socket.create_connection((board_ip, OTA_PORT), timeout=5):
        pass


def test_netlog_receives_from_board(board_ip, netlog_sock):
    """Netlog UDP path works board -> host (runbook "Logs").

    A healthy board is SILENT — netlog only broadcasts WARN+ lines
    (firmware/esp32-ccid/src/netlog.rs, log level Warn). So the test
    triggers one deterministic WARN: a bad-magic OTA probe makes ota.rs
    log "update failed: bad magic" and answer 'E' on :3232. This also
    proves the OTA error path leaves the board alive.
    """
    probe = socket.create_connection((board_ip, OTA_PORT), timeout=5)
    try:
        probe.sendall(b"XXXX" + (0).to_bytes(4, "little"))  # bad magic + size 0
        probe.settimeout(5)
        resp = probe.recv(1)
        assert resp == b"E", f"board did not reject bad magic with 'E': {resp!r}"
    finally:
        probe.close()

    deadline = time.time() + 10
    seen = []
    while time.time() < deadline:
        netlog_sock.settimeout(max(0.5, deadline - time.time()))
        try:
            data, addr = netlog_sock.recvfrom(2048)
        except socket.timeout:
            break
        seen.append((addr[0], data.decode(errors="replace").rstrip()))
        if addr[0] == board_ip:
            return  # >=1 WARN line from the board
    pytest.fail(f"no netlog line from {board_ip} within 10s (ufw 4567/udp? "
                f"AP not forwarding subnet broadcast? seen={seen})")


def test_vpcd_reader_slot_present(slot_map):
    """pcscd exposes the Nucula vpcd driver slot(s) (runbook "Host setup"
    reader.conf + "Multi-board benches")."""
    assert any(n.startswith("Nucula VPCD") for n in slot_map), slot_map


def test_card_atr(slot_map):
    """ATR over the full reversed chain: RF -> PN7160 -> vpcd TCP -> pcscd.

    Runbook rule 2: the ATR comes from a REAL SCardConnect, never from the
    GetStatusChange cache. Guarded by NUCULA_EXPECT_CARD (default 1):
    benches with an empty reader can still run the link tests.
    """
    if os.environ.get("NUCULA_EXPECT_CARD", "1") != "1":
        pytest.skip("NUCULA_EXPECT_CARD != 1 — card presence not expected")
    carded = {n: atr for n, atr in slot_map.items() if atr}
    assert carded, f"no Nucula slot returned an ATR (slot_map={slot_map})"
    atr = next(iter(carded.values()))
    assert atr.startswith(SMARTPGP_ATR_PREFIX), (
        f"unexpected ATR {atr} — bench card should be SmartPGP "
        f"({SMARTPGP_ATR_PREFIX}...)")
