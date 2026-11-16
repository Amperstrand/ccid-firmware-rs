#!/usr/bin/env python3
"""vpcd card-side server — host a CardBackend behind a REAL pcscd.

Takes any APDU-level card backend (the CardBackend surface used by
specter_sim_relay / specter_card_model: ``connect``/``disconnect``/
``exchange``) and exposes it through a vpcd reader slot, so completely
unmodified PC/SC consumers reach it:

    pyscard/OpenSC -> pcscd -> libifdvpcd --TCP--> vpcd_card_server -> backend

Wire role (the name says server, the wire says vpicc): vpcd — the
pcscd reader driver — is the TCP LISTENER on its configured port and
the VIRTUAL CARD connects out to it. That is the vpicc role this module
implements (same as vjavacard's VpcdBridge and the firmware vpcd
client). One session at a time; the driver's listener is backlog-1 and
single-client.

Protocol (vsmartcard "Creating a Virtual Smart Card",
frankmorgner.github.io/vsmartcard/virtualsmartcard/api.html; upstream
vpcd.c at the repo pin 93286a90d2411120ae17299f9902e1fdd8954185):
every message in BOTH directions is a 2-byte big-endian length prefix
followed by exactly that many payload bytes; vpcd initiates everything.

    len==1 control:  0x00 PowerOff -> NO reply (an empty ack desyncs
                                   the driver's next ATR probe)
                      0x01 PowerOn  -> NO reply
                      0x02 Reset    -> NO reply (pcscd follows Reset
                                   unconditionally with GetATR; also
                                   answering here queues a stale ATR
                                   that desyncs the next transmit's
                                   status word — VpcdBridge bench
                                   lesson, SW=1084 class)
                      0x04 GetATR   -> reply the ATR, length-prefixed
    len>1:            C-APDU        -> backend.exchange() -> R-APDU

A zero-length ATR is the protocol's "no card" signal; this server only
speaks when spoken to, so it never emits one.

Bench operational rules that shaped this code (AGENTS.md / the vjavacard
lane): never run two vpiccs against one slot port and never TCP-probe a
slot to "check readiness" — parse /proc/net/tcp instead (backlog-1
displacement kills a live bridge). A quiet socket is a healthy socket:
read timeouts are tolerated, only EOF/error ends a session, after which
we reconnect like a re-seated card.
"""
from __future__ import annotations

import argparse
import socket
import struct
import sys
import threading
import time
from pathlib import Path

# vpcd control bytes (upstream vpcd.h: VPCD_CTRL_OFF/ON/RESET/GET_ATR)
CTRL_OFF = 0x00
CTRL_ON = 0x01
CTRL_RESET = 0x02
CTRL_GET_ATR = 0x04

#: Every socket operation is bounded — a hung accept()/recv() would
#: wedge any test or CLI that hosts this server (task constraint: 5 s).
IO_TIMEOUT = 5.0
CONNECT_RETRY_DELAY = 0.25

#: Fallback ATR when the backend exposes none: the ComSign eID the
#: MemoryCardModel stands in for (bench host table in AGENTS.md —
#: T=1, IFSC=254, TCK correct).
DEFAULT_ATR = bytes.fromhex("3BD518FF8191FE1FC38073C821100A")

SW_NO_DIAGNOSIS = b"\x6f\x00"  # ISO 7816: no precise diagnosis


def _call(backend, method: str) -> None:
    """Best-effort backend notification; never breaks the session."""
    fn = getattr(backend, method, None)
    if fn is None:
        return
    try:
        fn()
    except Exception as exc:  # noqa: BLE001 — session must survive
        print(f"vpcd_card_server: backend.{method}() raised: {exc!r}",
              file=sys.stderr)


class VpcdCardServer:
    """Host one card backend behind a vpcd reader slot.

    ``start()`` spawns the worker thread: it retries the outbound
    connect until vpcd's listener appears (start it BEFORE pcscd and
    the card is present at the driver's very first probe — this
    sidesteps the stock driver's absent-probe event-thread death, the
    #349 class), then serves vpcd-initiated exchanges until ``stop()``.
    """

    def __init__(self, backend, host: str = "127.0.0.1", port: int = 28350,
                 atr: bytes | None = None,
                 io_timeout: float = IO_TIMEOUT):
        self.backend = backend
        self.host = host
        self.port = port
        self.io_timeout = io_timeout
        if atr is None:
            atr = getattr(backend, "atr", None)
        if not isinstance(atr, (bytes, bytearray)):
            # missing/malformed backend ATR -> ComSign eID shape
            atr = DEFAULT_ATR
        self.atr = bytes(atr)
        self._stop = threading.Event()
        self._attached = threading.Event()
        self._thread: threading.Thread | None = None
        self._sock: socket.socket | None = None
        self.sessions = 0  # completed vpcd session count (diagnostics)

    # -- lifecycle ------------------------------------------------------

    def start(self) -> None:
        if self._thread is not None and self._thread.is_alive():
            return
        self._stop.clear()
        self._thread = threading.Thread(
            target=self._run, name="vpcd-card-server", daemon=True)
        self._thread.start()

    def stop(self, timeout: float | None = None) -> None:
        """Idempotent shutdown; interrupts a blocking read."""
        self._stop.set()
        sock = self._sock
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass
        if self._thread is not None:
            self._thread.join(timeout=timeout or 2 * self.io_timeout + 1)

    def wait_attached(self, timeout: float = 10.0) -> bool:
        """True once a vpcd session is established (card 'inserted')."""
        return self._attached.wait(timeout)

    # -- worker ---------------------------------------------------------

    def _run(self) -> None:
        while not self._stop.is_set():
            try:
                sock = socket.create_connection(
                    (self.host, self.port), timeout=self.io_timeout)
            except OSError:
                # vpcd listener not up yet (pcscd still loading the
                # driver) — retry quietly, never touch other ports.
                time.sleep(CONNECT_RETRY_DELAY)
                continue
            sock.settimeout(self.io_timeout)
            self._sock = sock
            self._attached.set()
            try:
                self.serve(sock)
            finally:
                self.sessions += 1
                self._attached.clear()
                self._sock = None
                try:
                    sock.close()
                except OSError:
                    pass
            if not self._stop.is_set():
                # re-seated card: give a restarting pcscd a beat before
                # hammering its backlog-1 listener again
                time.sleep(CONNECT_RETRY_DELAY)

    def serve(self, sock: socket.socket) -> None:
        """One vpcd session: answer vpcd-initiated messages until EOF.

        Public on purpose — the offline protocol tests drive this loop
        directly over a socketpair, no pcscd involved.
        """
        while not self._stop.is_set():
            msg = self._recv_msg(sock)
            if msg is None:  # EOF / hard error: session over
                return
            if msg == b"":  # zero-length: ignore, never answer
                continue
            if len(msg) == 1:
                ctrl = msg[0]
                if ctrl == CTRL_GET_ATR:
                    if not self._send_msg(sock, self.atr):
                        return
                elif ctrl == CTRL_OFF:
                    _call(self.backend, "disconnect")
                elif ctrl == CTRL_ON:
                    _call(self.backend, "connect")
                elif ctrl == CTRL_RESET:
                    # warm reset: session state dies, transport survives
                    _call(self.backend, "disconnect")
                    _call(self.backend, "connect")
                # OFF/ON/RESET and unknown controls: NO reply (above)
                continue
            if not self._send_msg(sock, self._exchange(msg)):
                return

    def _exchange(self, capdu: bytes) -> bytes:
        try:
            rapdu = self.backend.exchange(bytes(capdu))
        except Exception as exc:  # noqa: BLE001 — 6F00, session survives
            print(f"vpcd_card_server: backend.exchange() raised: {exc!r}",
                  file=sys.stderr)
            return SW_NO_DIAGNOSIS
        if not isinstance(rapdu, bytes):
            rapdu = bytes(rapdu)
        if len(rapdu) < 2:
            # a card must always answer at least a status word
            return SW_NO_DIAGNOSIS
        return rapdu

    # -- framed I/O -----------------------------------------------------

    def _recv_exact(self, sock: socket.socket, n: int) -> bytes | None:
        buf = b""
        while len(buf) < n:
            if self._stop.is_set():
                return None
            try:
                chunk = sock.recv(n - len(buf))
            except TimeoutError:
                # a quiet socket is a healthy card (VpcdBridge
                # transmit-dead-window lesson) — keep waiting
                continue
            except OSError:
                return None
            if not chunk:
                return None  # EOF
            buf += chunk
        return buf

    def _recv_msg(self, sock: socket.socket) -> bytes | None:
        hdr = self._recv_exact(sock, 2)
        if hdr is None:
            return None
        (length,) = struct.unpack(">H", hdr)
        if length == 0:
            return b""
        return self._recv_exact(sock, length)

    def _send_msg(self, sock: socket.socket, payload: bytes) -> bool:
        try:
            sock.settimeout(self.io_timeout)
            sock.sendall(struct.pack(">H", len(payload)) + payload)
            return True
        except OSError:
            return False


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Host the Specter MemoryCard model behind a vpcd "
                    "reader slot (connects OUT to the vpcd listener).")
    parser.add_argument("--host", default="127.0.0.1",
                        help="host running pcscd/vpcd (default 127.0.0.1)")
    parser.add_argument("--port", type=int, default=28350,
                        help="vpcd slot port from reader.conf DEVICENAME "
                             "(default 28350)")
    parser.add_argument("--atr", default=None,
                        help="hex ATR override (default: backend ATR / "
                             "ComSign eID shape)")
    args = parser.parse_args(argv)

    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from specter_card_model import MemoryCardModel

    atr = bytes.fromhex(args.atr) if args.atr else None
    server = VpcdCardServer(MemoryCardModel(), host=args.host,
                            port=args.port, atr=atr)
    server.start()
    print(f"vpcd_card_server: MemoryCardModel -> {args.host}:{args.port} "
          f"(ATR {server.atr.hex().upper()}), Ctrl-C to stop")
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        pass
    finally:
        server.stop()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
