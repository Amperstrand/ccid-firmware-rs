#!/usr/bin/env python3
"""Mock nucula wifi-CCID board for host-side driver tests (issue #157).

Speaks the GemPC Twin serial framing the firmware serves on TCP :29443
(crates/ccid-transport-serial): every received frame is ECHOED byte-exact,
then answered with the response frame. Single client at a time; the
mock answers from state, no hardware.

Models the firmware's actual semantics (mock/hardware-divergence doctrine):
- silent drop on bad LRC (wifi ServeConfig: no NAK)
- 0x62 -> DataBlock with the G-05 ATR (3B 85 81 31 FE 45 80 73 C8 21 10 84)
- 0x6F -> DataBlock with a canned R-APDU (90 00 by default)
- bSeq echoed in every response (CCID 6.2-1)
- SetParameters accepted (5/7-byte shape checked like the firmware)

Usage: mock_board.py [port] [--absent] [--rapdu <hex>]
"""
import socket
import sys
import threading

SYNC, ACK, NAK = 0x03, 0x06, 0x15
PC_POWER_ON, PC_POWER_OFF, PC_SLOT_STATUS = 0x62, 0x63, 0x65
PC_XFR_BLOCK, PC_GET_PARAMS, PC_SET_PARAMS, PC_ESCAPE = 0x6F, 0x6C, 0x61, 0x6B
RDR_DATA_BLOCK, RDR_SLOT_STATUS, RDR_PARAMS, RDR_ESCAPE = 0x80, 0x81, 0x82, 0x83

# G-05 ATR as synthesized by the firmware (pn7160-nci transport.rs test vector)
ATR = bytes.fromhex("3B858131FE458073C8211084")
IDENT = b"Nucula Mock 1.0\x00"


def lrc(buf: bytes) -> int:
    x = 0
    for b in buf:
        x ^= b
    return x


def frame(msg: bytes) -> bytes:
    return bytes([SYNC, ACK]) + msg + bytes([lrc(bytes([SYNC, ACK]) + msg)])


def hdr(mtype: int, seq: int, status: int, error: int, spec: int, plen: int) -> bytes:
    return bytes([mtype, plen & 0xFF, (plen >> 8) & 0xFF, (plen >> 16) & 0xFF,
                  (plen >> 24) & 0xFF, 0x00, seq, status, error, spec])


def read_exact(conn, n):
    buf = b""
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


def read_frame(conn):
    """Sync-anchored scan, mirrors the firmware FrameParser."""
    while True:
        b = read_exact(conn, 1)
        if b is None:
            return None
        if b[0] != SYNC:
            continue
        c = read_exact(conn, 1)
        if c is None:
            return None
        if c[0] == NAK:
            continue
        if c[0] != ACK:
            continue
        head = read_exact(conn, 10)
        if head is None:
            return None
        plen = int.from_bytes(head[1:5], "little")
        payload = read_exact(conn, plen) if plen else b""
        if payload is None:
            return None
        body = bytes([SYNC, ACK]) + head + payload
        l = read_exact(conn, 1)
        if l is None:
            return None
        if l[0] != lrc(body):
            continue  # bad LRC: silent drop, keep connection (wifi profile)
        return head, payload


class MockBoard:
    def __init__(self, absent=False, rapdu=bytes([0x90, 0x00])):
        self.absent = absent
        self.powered = False
        self.rapdu = rapdu
        self.lock = threading.Lock()

    def status_icc(self):
        if self.absent:
            return 0x02  # NO_ICC
        return 0x00 if self.powered else 0x01  # active / inactive

    def handle(self, head, payload):
        mtype, seq = head[0], head[6]
        if self.absent:
            # firmware: no card -> failed slot-status-style answers
            return frame(hdr(RDR_SLOT_STATUS, seq, (1 << 6) | 0x02, 0xFE, 0, 0))
        if mtype == PC_POWER_ON:
            self.powered = True
            return frame(hdr(RDR_DATA_BLOCK, seq, 0x00, 0, 0, len(ATR)) + ATR)
        if mtype == PC_POWER_OFF:
            self.powered = False
            return frame(hdr(RDR_SLOT_STATUS, seq, 0x01, 0, 0, 0))
        if mtype == PC_SLOT_STATUS:
            return frame(hdr(RDR_SLOT_STATUS, seq, self.status_icc(), 0, 0, 0))
        if mtype == PC_XFR_BLOCK:
            return frame(hdr(RDR_DATA_BLOCK, seq, 0x00, 0, 0, len(self.rapdu))
                         + self.rapdu)
        if mtype == PC_GET_PARAMS:
            p = bytes([0x11, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00])
            return frame(hdr(RDR_PARAMS, seq, 0x00, 0, 1, len(p)) + p)
        if mtype == PC_SET_PARAMS:
            if len(payload) not in (5, 7):
                return frame(hdr(RDR_SLOT_STATUS, seq, (1 << 6) | self.status_icc(),
                                 0x07, 0, 0))
            return frame(hdr(RDR_PARAMS, seq, 0x00, 0, head[7], len(payload))
                         + payload)
        if mtype == PC_ESCAPE:
            if payload[:1] == b"\x02":
                return frame(hdr(RDR_ESCAPE, seq, 0x00, 0, 0, len(IDENT)) + IDENT)
            if payload[:1] == b"\xD0":
                counters = bytes(28)
                return frame(hdr(RDR_ESCAPE, seq, 0x00, 0, 0, 28) + counters)
            return frame(hdr(RDR_ESCAPE, seq, 0x00, 0, 0, len(payload)) + payload)
        return frame(hdr(RDR_SLOT_STATUS, seq, (1 << 6) | self.status_icc(), 0x00,
                         0, 0))


def serve(port, board):
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", port))
    srv.listen(1)
    print(f"mock board on 127.0.0.1:{port} (absent={board.absent})", flush=True)
    while True:
        conn, peer = srv.accept()
        print(f"client {peer}", flush=True)
        with board.lock:
            try:
                while True:
                    got = read_frame(conn)
                    if got is None:
                        break
                    head, payload = got
                    body = bytes([SYNC, ACK]) + head + payload
                    conn.sendall(body + bytes([lrc(body)]))  # echo first
                    conn.sendall(board.handle(head, payload))
            except ConnectionError:
                pass
        conn.close()
        print("client gone; re-arming", flush=True)


def main():
    port, absent, rapdu = 29444, False, bytes([0x90, 0x00])
    args = sys.argv[1:]
    if args and args[0].isdigit():
        port = int(args.pop(0))
    if "--absent" in args:
        absent = True
    if "--rapdu" in args:
        i = args.index("--rapdu")
        rapdu = bytes.fromhex(args[i + 1])
    serve(port, MockBoard(absent, rapdu))


if __name__ == "__main__":
    main()
