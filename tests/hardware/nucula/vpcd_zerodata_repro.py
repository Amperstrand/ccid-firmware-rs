#!/usr/bin/env python3
"""vpcd zero-data APDU repro harness (issue #170, bench leg).

Drives the nucula wifi-vpcd relay with the bench-exact byte sequence
that wedged the card-session (SatoCash v0.1, 2026-10-14 evidence):

  ATR request -> 5-byte case-2 soak (stable baseline) -> the bare
  6-byte zero-data APDU B0 B0 00 00 00 00 (Lc=0 before Le) -> the
  card answers 6700 -> post-answer probe exchanges on both slots.

Acts as the HOST side of the vpcd protocol (vsmartcard vpcd.c): the
board connects OUT to this listener (reversed mode), all messages are
2-byte big-endian length + payload; control = 1-byte payload
(0x04 ATR, 0x02 RESET, 0x01 ON, 0x00 OFF); APDU = full C-APDU, reply =
full R-APDU; 0-length reply = "ICC absent" (driver ejects + re-polls).

Wedge signature (pre-fix): after the 6700, every subsequent exchange
returns the cached ATR bytes as payload with garbage SW; pcscd restart
does not heal, same-image OTA reboot does.

Post-fix expectation: the session-fatal failure answers 0-length and
the driver-style reconnect (ATR re-request) rebuilds the session.

Usage (bench host, board on the coil, wifi-vpcd image running):
  python3 vpcd_zerodata_repro.py                 # default port 35963
  python3 vpcd_zerodata_repro.py --port 35964    # second board/slot
  python3 vpcd_zerodata_repro.py --skip-wedge    # baseline soak only

Exit codes: 0 = clean (no wedge), 1 = wedge signature detected,
2 = environment error (no board connected in time).
"""
import argparse
import socket
import struct
import sys
import time

HOST_VPCD_PORT = 35963
VPCD_CTRL_ATR = 0x04
CONNECT_TIMEOUT_S = 30.0
IO_TIMEOUT_S = 10.0

ZERO_DATA_APDU = bytes([0xB0, 0xB0, 0x00, 0x00, 0x00, 0x00])
CASE2_APDU = bytes([0xB0, 0xB0, 0x00, 0x00, 0x00])


def recv_msg(sock):
    hdr = recv_exact(sock, 2)
    if hdr is None:
        return None
    (n,) = struct.unpack(">H", hdr)
    if n == 0:
        return b""
    return recv_exact(sock, n)


def recv_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


def send_msg(sock, payload):
    sock.sendall(struct.pack(">H", len(payload)) + payload)


def exchange(sock, apdu, timeout_label):
    t0 = time.monotonic()
    send_msg(sock, apdu)
    rsp = recv_msg(sock)
    dt_ms = (time.monotonic() - t0) * 1000.0
    if rsp is None:
        print(f"  {timeout_label}: NO RESPONSE (connection lost) "
              f"after {dt_ms:.0f} ms")
        return None
    print(f"  {timeout_label}: {rsp.hex(' ')} ({len(rsp)} B, {dt_ms:.0f} ms)")
    return rsp


def looks_like_wedge(rsp, atr):
    if rsp is None or len(rsp) < 2:
        return False
    payload, sw = rsp[:-2], rsp[-2:]
    return bool(atr) and payload == atr and sw not in (b"\x90\x00", b"\x67\x00")


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", type=int, default=HOST_VPCD_PORT)
    ap.add_argument("--soak", type=int, default=20,
                    help="5-byte case-2 baseline exchanges")
    ap.add_argument("--probes", type=int, default=5,
                    help="post-wedge exchanges per slot")
    ap.add_argument("--skip-wedge", action="store_true",
                    help="baseline soak only, never send the 6-byte form")
    args = ap.parse_args()

    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("0.0.0.0", args.port))
    srv.listen(1)
    print(f"listening on :{args.port} — power-cycle or reboot the board "
          f"so its vpcd client reconnects here")
    srv.settimeout(CONNECT_TIMEOUT_S)
    try:
        sock, addr = srv.accept()
    except socket.timeout:
        print(f"no board connected within {CONNECT_TIMEOUT_S:.0f} s")
        return 2
    sock.settimeout(IO_TIMEOUT_S)
    print(f"board connected from {addr}")

    send_msg(sock, bytes([VPCD_CTRL_ATR]))
    atr = recv_msg(sock)
    if atr is None or not atr:
        print("no ATR — card not on the coil?")
        return 2
    print(f"ATR: {atr.hex(' ')}")

    def exchange_or_reconnect(sock_ref, apdu, label):
        """Codex P2 on #232: a 0-length APDU answer is the vpcd.h
        ICC-absent signal — the stock host driver EJECTS the reader and
        reconnects (fresh ATR) rather than probing on. Model exactly
        that: on b"" close the socket, accept the board's reconnect,
        request ATR, and only then report. A board that answers empty
        but then serves a FRESH ATR and clean exchanges is HEALED, not
        wedged — the harness must not conflate the two.

        Codex P2 on #232 (issue #436): a failed reconnect leaves the
        closed fd replaced by a None sentinel — every later exchange
        and the unconditional final ATR probe must check it instead of
        raising OSError (Bad file descriptor) on the dead descriptor."""
        if sock_ref[0] is None:
            print(f"{label}: SKIP — no live connection "
                  "(reconnect failed earlier)")
            return None
        rsp = exchange(sock_ref[0], apdu, label)
        if rsp == b"":
            print(f"{label}: ICC-absent (0-length) — modeling the host "
                  f"eject + reconnect")
            sock_ref[0].close()
            srv.settimeout(CONNECT_TIMEOUT_S)
            try:
                sock_ref[0], _ = srv.accept()
            except socket.timeout:
                print("no reconnect after ICC-absent — board did not "
                      "re-attach (report this: worse than the wedge)")
                sock_ref[0] = None
                return None
            sock_ref[0].settimeout(IO_TIMEOUT_S)
            send_msg(sock_ref[0], bytes([VPCD_CTRL_ATR]))
            fresh = recv_msg(sock_ref[0])
            print(f"reconnect ATR: {fresh.hex(' ') if fresh else fresh!r}")
            return fresh
        return rsp

    sock_ref = [sock]
    clean = True
    for i in range(args.soak):
        rsp = exchange_or_reconnect(sock_ref, CASE2_APDU, f"case2[{i}]")
        if rsp is None or not rsp.endswith(b"\x90\x00"):
            print("baseline soak failed before the zero-data probe")
            clean = False
            break

    if clean and not args.skip_wedge:
        rsp = exchange(sock, ZERO_DATA_APDU, "zero-data")
        if rsp is not None and not rsp.endswith(b"\x67\x00"):
            print(f"NOTE: expected the card's 6700, got {rsp.hex(' ')}")

        for i in range(args.probes):
            rsp = exchange_or_reconnect(sock_ref, CASE2_APDU, f"post[{i}]")
            if rsp is None:
                # Codex P2 on #487: a failed post-probe exchange (dead
                # socket, failed reconnect) is an UNSUCCESSFUL outcome,
                # not a pass — the repro must not print CLEAN over a
                # lost board.
                print(f"POST-PROBE LOST: post[{i}] no response — marking "
                      f"unsuccessful")
                clean = False
            elif looks_like_wedge(rsp, atr):
                print(f"WEDGE: post[{i}] served ATR bytes with garbage SW "
                      f"{rsp[-2:].hex(' ')}")
                clean = False

    # #436: the final ATR probe runs only on a LIVE socket — after a
    # failed reconnect sock_ref[0] is None (the old fd is closed), and
    # an unconditional send_msg there raised an uncaught OSError instead
    # of one of the documented outcomes.
    if sock_ref[0] is not None:
        send_msg(sock_ref[0], bytes([VPCD_CTRL_ATR]))
        atr2 = recv_msg(sock_ref[0])
        print(f"post-probe ATR: {atr2.hex(' ') if atr2 else atr2!r}")
        sock_ref[0].close()
    else:
        print("post-probe ATR: skipped — no live connection "
              "(reconnect failed)")
    srv.close()
    print("RESULT:", "CLEAN" if clean else "WEDGE SIGNATURE")
    return 0 if clean else 1


if __name__ == "__main__":
    sys.exit(main())
