#!/usr/bin/env python3
"""vpicc_table — scripted ISO-7816 card over the vpcd wire (#261 tier a).

A software PICC that connects OUT to a vpcd listener (pcscd side) and
answers every APDU from a fixed, self-marked table — zero hardware, so
GP-level reader/tooling tests run in CI or on a busy bench.

Wire protocol (vsmartcard vpcd; mirrored from our firmware's
client_loop): every message = 2-byte big-endian length + payload.
Driver→card messages are either 1-byte controls (0x00 OFF, 0x01 ON,
0x02 RESET, 0x04 ATR) or a full C-APDU. Card answers: ATR control →
the ATR bytes (empty = ICC absent); APDU → the R-APDU from the table.

Self-marking doctrine (#261): every FCI/data answer carries the
'VPICC-TABLE' marker and synthetic filler (A5A5/5A5A) so no trace can
be mistaken for a real SE — mirrors the vcard convention.
"""

from __future__ import annotations

import argparse
import socket
import threading
import time

# Bench J3R180 contactless shape (12B) — evidence-grade ATR, synthetic body.
DEFAULT_ATR = "3B858131FE458073C8211084"
MARKER = b"VPICC-TABLE"


def _tlv(tag: int, value: bytes) -> bytes:
    assert len(value) < 128
    return bytes([tag, len(value)]) + value


def _fci(aid: bytes) -> bytes:
    return _tlv(0x6F, _tlv(0x84, aid) + _tlv(0x79, b"\x60") + _tlv(0x90, MARKER))


def default_table() -> dict:
    """A GP-smoke table: enough for SELECT/auth-echo/status/load/install."""
    fci = _fci(bytes.fromhex("A000000151000000"))
    # INITIALIZE UPDATE answer: key ver 00, SC 0000, filler challenges,
    # synthetic card cryptogram (A5/5A filler — never real crypto).
    iu = bytes([0x00, 0x00, 0x00]) + b"\xA5" * 8 + b"\x5A" * 8
    cplc = bytes.fromhex("9F7F2D") + b"\xA5" * 0x2D
    return {
        # (CLA, INS) prefix keys; None component = wildcard.
        (0x00, 0xA4): fci + b"\x90\x00",          # SELECT (any AID path)
        (0x80, 0x50): iu + b"\x90\x00",           # INITIALIZE UPDATE
        (0x84, 0x82): b"\x90\x00",                # EXTERNAL AUTHENTICATE
        (0x80, 0xCA): cplc + b"\x90\x00",         # GET DATA (CPLC-shaped)
        (0x80, 0xF2): b"\x90\x00",                # GET STATUS (minimal)
        (0x80, 0xE8): b"\x90\x00",                # LOAD
        (0x80, 0xE6): b"\x90\x00",                # INSTALL
        (0x80, 0xE4): b"\x90\x00",                # DELETE
    }


ISD_AID = bytes.fromhex("A0000001510000")


def answer(table: dict, capdu: bytes) -> bytes:
    if len(capdu) < 4:
        return b"\x67\x00"
    if capdu[0] == 0x00 and capdu[1] == 0xA4:
        # SELECT is AID-aware: only the ISD gets the FCI; any other
        # AID is file-not-found (the wildcard table entry would
        # otherwise 9000 every SELECT).
        lc = capdu[4] if len(capdu) > 4 else 0
        aid = capdu[5:5 + lc]
        if aid == ISD_AID:
            return table[(0x00, 0xA4)]
        return b"\x6A\x82"
    for (cla, ins), resp in table.items():
        if capdu[0] == cla and capdu[1] == ins:
            return resp
    return b"\x6A\x82"  # unknown — file/aid not found


class TableVpicc(threading.Thread):
    """Connects out to a vpcd listener and serves the table.

    daemon=True; call .stop() (or del) to end the session loop.
    """

    def __init__(self, host: str, port: int, atr_hex: str | None = None,
                 table: dict | None = None):
        super().__init__(daemon=True)
        self.host = host
        self.port = port
        self.atr = bytes.fromhex(atr_hex or DEFAULT_ATR)
        self.table = table if table is not None else default_table()
        self._stop = threading.Event()
        self.apdus_seen: list[bytes] = []
        self.answered_probe = threading.Event()  # first ATR probe served

    def stop(self) -> None:
        self._stop.set()

    def run(self) -> None:  # pragma: no cover - exercised via tests
        while not self._stop.is_set():
            try:
                sock = socket.create_connection((self.host, self.port), timeout=2)
            except OSError:
                time.sleep(0.5)
                continue
            try:
                self._session(sock)
            except OSError:
                pass
            finally:
                sock.close()
            time.sleep(0.2)

    def _session(self, sock: socket.socket) -> None:
        sock.settimeout(5.0)
        buf = b""
        while not self._stop.is_set():
            try:
                chunk = sock.recv(4096)
            except socket.timeout:
                continue
            if not chunk:
                return  # driver closed
            buf += chunk
            while len(buf) >= 2:
                n = int.from_bytes(buf[:2], "big")
                if len(buf) < 2 + n:
                    break
                msg, buf = buf[2:2 + n], buf[2 + n:]
                self._handle(sock, msg)

    def _handle(self, sock: socket.socket, msg: bytes) -> None:
        if len(msg) == 1:
            # Controls: RESET and ATR want the ATR back; ON/OFF get a
            # 0-length ack (the stock driver expects a reply to every
            # control — silence only works on our patched driver).
            if msg[0] in (0x02, 0x04):
                # RESET and ATR want the ATR back.
                self._send(sock, self.atr)
                self.answered_probe.set()
            # OFF and ON get NO reply — mirror the firmware client
            # exactly: an empty ack here de-syncs the driver's next
            # ATR probe (read as 0-length = ICC absent → eject → the
            # slot dies for good). Fire-and-forget, like vpcd.rs.
            return
        self.apdus_seen.append(msg)
        self._send(sock, answer(self.table, msg))

    @staticmethod
    def _send(sock: socket.socket, payload: bytes) -> None:
        sock.sendall(len(payload).to_bytes(2, "big") + payload)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=35970)
    ap.add_argument("--atr", default=DEFAULT_ATR)
    args = ap.parse_args()
    card = TableVpicc(args.host, args.port, args.atr)
    card.start()
    print(f"vpicc_table: serving {args.host}:{args.port} "
          f"(ATR {args.atr}), Ctrl-C to stop")
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        card.stop()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
