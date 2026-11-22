#!/usr/bin/env python3
"""End-to-end CLI test: hce_relay against a FAKE PHONE — no adb, no S20.

The fake phone plays the app side of the VpcdBridge socket: accepts the
brain's connection, says hello (0x02), forwards one scripted C-APDU,
validates the R-APDU, announces deactivation (0x01), closes. The real
hce_relay.py runs as a subprocess through its actual CLI (--connect-tcp
direct transport) — the full main() path, zero hardware, zero adb.
"""
import socket
import subprocess
import sys
import threading
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
HOST_TOOLS = HERE.parents[2] / "host-tools"

CTRL_DEACTIVATED = b"\x01"
CTRL_HELLO = b"\x02"
SELECT = bytes.fromhex("00A4040007D276000085010100")


def recv_frame(s: socket.socket) -> bytes:
    header = s.recv(2)
    if len(header) < 2:
        raise EOFError("peer closed")
    n = int.from_bytes(header, "big")
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            raise EOFError("peer closed mid-frame")
        buf += chunk
    return buf


def send_frame(s: socket.socket, payload: bytes) -> None:
    s.sendall(len(payload).to_bytes(2, "big") + payload)


class FakePhone(threading.Thread):
    """Listener that plays the phone: one connection, one exchange."""

    def __init__(self):
        super().__init__(daemon=True)
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(1)
        self.port = self.sock.getsockname()[1]
        self.seen: list[bytes] = []
        self.ok = False

    def run(self):
        conn, _ = self.sock.accept()
        try:
            send_frame(conn, CTRL_HELLO)
            send_frame(conn, SELECT)
            rapdu = recv_frame(conn)
            self.seen.append(rapdu)
            self.ok = rapdu == bytes.fromhex("9000")
            send_frame(conn, CTRL_DEACTIVATED)
        finally:
            conn.close()
            self.sock.close()


@pytest.mark.timeout(30)
def test_cli_end_to_end_fake_phone(tmp_path):
    phone = FakePhone()
    phone.start()
    journal = tmp_path / "j.jsonl"
    r = subprocess.run(
        [sys.executable, str(HOST_TOOLS / "hce_relay.py"),
         "--connect-tcp", f"127.0.0.1:{phone.port}",
         "--respond", "9000", "--once", "1",
         "--journal", str(journal)],
        capture_output=True, text=True, timeout=25)
    phone.join(timeout=10)
    assert r.returncode == 0, r.stderr[-400:]
    assert phone.ok, f"fake phone saw: {[b.hex() for b in phone.seen]}"
    assert "connected (direct" in r.stdout
    assert journal.exists()  # the session journaled the exchange
