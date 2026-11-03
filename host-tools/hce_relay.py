#!/usr/bin/env python3
"""hce_relay.py — host-side card brain for the bench HCE relay (#529).

The phone (com.amperstrand.benchrelay, host-tools/hce-relay-app) is an
RF modem: it forwards every C-APDU the reader sends to this process and
returns whatever we answer. Frames: 2-byte BIG-ENDIAN length + payload
(the org.amperstrand.VpcdBridge framing). Control frames from the phone:
0x01 = ISO-DEP link deactivated (field off / re-poll), 0x02 = hello.

Transport: adb forward tcp:4871 localabstract:bench-hce-relay

Backends (where R-APDUs come from):
  static            answer every C-APDU with --respond HEX (default 9000)
  script            table from --script FILE (`-` = stdin); miss -> --miss
  pcsc              real card in a PC/SC reader (--reader NEEDLE): the
                    live-card relay — reader-side card answers through
                    the phone's HCE. Read-only allowlist discipline
                    applies to whatever the caller drives through it.

Bench instruments (the #529 modes):
  --delay-ms N      sleep N ms before answering (the #472 slow-card /
                    time-extension instrument; also WTX-timing tests)
  --once N          exit after N exchanges
  --journal FILE    JSONL evidence journal (ts, event, len, sha8; raw
                    hex only with --log-hex — sensitive replies stay
                    out of artifacts by default)
  --meta k=v        recorded in the journal session header (commit,
                    firmware hash, fixture geometry ...)

Session doctrine (the #529-kickoff contract): one phone socket is bound
to ONE backend card session. The backend connects lazily on the first
C-APDU (card activation) and disconnects when the phone link drops. A
backend failure is an EXPLICIT error — the session dies, the phone
socket closes, nothing is retried, and no reply is ever paired with a
C-APDU it did not answer.

Place/remove verbs (see #529): svc nfc enable/disable on the phone.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import socket
import subprocess
import sys
import time
from abc import ABC, abstractmethod
from typing import Optional

HOST_PORT = 4871
SOCKET_NAME = "bench-hce-relay"
MAX_FRAME = 0xFFFF  # 2-byte BE length header caps a frame at 65535


class RelayError(RuntimeError):
    """Base error: surfaced as an explicit session failure, never silent."""


class BackendError(RelayError):
    """The card backend failed mid-session (card gone, reader error, ...)."""


# --- framing (org.amperstrand.VpcdBridge: 2-byte BE length + payload) -------


def recv_exact(s: socket.socket, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            raise EOFError("peer closed the relay socket")
        buf += chunk
    return buf


def recv_frame(s: socket.socket) -> bytes:
    header = recv_exact(s, 2)
    return recv_exact(s, int.from_bytes(header, "big"))


def send_frame(s: socket.socket, payload: bytes) -> None:
    if len(payload) > MAX_FRAME:
        raise RelayError(f"frame too large: {len(payload)} > {MAX_FRAME}")
    s.sendall(len(payload).to_bytes(2, "big") + payload)


# --- script tables -----------------------------------------------------------


def parse_script(lines, source: str = "<script>") -> dict:
    """Parse CAPDU_HEX=RAPDU_HEX lines. Loud on anything ambiguous."""
    table: dict = {}
    for lineno, raw in enumerate(lines, 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if "=" not in line:
            raise RelayError(f"{source}: line {lineno}: missing '=' "
                             f"(expected CAPDU_HEX=RAPDU_HEX): {line!r}")
        capdu_hex, rapdu_hex = (part.strip() for part in line.split("=", 1))
        try:
            capdu = bytes.fromhex(capdu_hex)
            rapdu = bytes.fromhex(rapdu_hex)
        except ValueError as exc:
            raise RelayError(f"{source}: line {lineno}: bad hex ({exc})") from None
        if capdu in table:
            raise RelayError(
                f"{source}: line {lineno}: duplicate C-APDU "
                f"{capdu.hex()} (first defined earlier)")
        if len(rapdu) < 2:
            raise RelayError(
                f"{source}: line {lineno}: R-APDU shorter than a status word")
        table[capdu] = rapdu
    return table


def load_script(path: str, stdin=None) -> dict:
    """Load a script table; `-` reads stdin (the RUNBOOK heredoc form)."""
    if path == "-":
        text = stdin if isinstance(stdin, str) else None
        if text is None:
            text = sys.stdin.read()
        return parse_script(text.splitlines(), source="<stdin>")
    with open(path, encoding="utf-8") as fh:
        return parse_script(fh, source=path)


# --- backends ----------------------------------------------------------------


class CardBackend(ABC):
    """One backend = one card session. Sequential exchanges only."""

    name = "abstract"

    def connect(self) -> None:  # card activation
        pass

    def disconnect(self) -> None:
        pass

    @abstractmethod
    def exchange(self, capdu: bytes) -> bytes:
        """Answer one C-APDU. Raises BackendError on card-side failure."""

    def status(self) -> dict:
        return {"name": self.name}


class StaticBackend(CardBackend):
    """Answer everything with a fixed R-APDU (the smoke-test card)."""

    name = "static"

    def __init__(self, fixed: bytes):
        if len(fixed) < 2:
            raise RelayError("static response shorter than a status word")
        self.fixed = fixed

    def exchange(self, capdu: bytes) -> bytes:
        return self.fixed


class ScriptBackend(CardBackend):
    """Canned table; miss -> configurable SW (default 6A82 file not found)."""

    name = "script"

    def __init__(self, table: dict, miss: bytes = b"\x6a\x82"):
        self.table = table
        self.miss = miss

    def exchange(self, capdu: bytes) -> bytes:
        return self.table.get(capdu, self.miss)

    def status(self) -> dict:
        return {"name": self.name, "entries": len(self.table)}


class PcscBackend(CardBackend):
    """Real card in a PC/SC reader: the live-card relay backend.

    One reader (by name substring), one connection, no retries: any
    card/reader error raises BackendError and the session dies loudly —
    the doctrine forbids re-pairing replies across a failed exchange or
    silently reconnecting to a different card instance.
    """

    name = "pcsc"

    def __init__(self, reader_needle: str):
        self.needle = reader_needle
        self._conn = None
        self._atr: Optional[bytes] = None

    def connect(self) -> None:
        try:
            from smartcard.System import readers
        except ImportError as exc:
            raise RelayError(
                "pyscard is not available in this python — run on the "
                "bench host/venv that has it, or use the scripted "
                "backend") from exc
        all_readers = [str(r) for r in readers()]
        matches = [r for r in all_readers if self.needle.lower() in r.lower()]
        if not matches:
            raise BackendError(f"no PC/SC reader matches {self.needle!r}; "
                               f"present: {all_readers}")
        if len(matches) > 1:
            raise BackendError(f"reader needle {self.needle!r} is ambiguous: "
                               f"{matches}")
        conn = readers()[all_readers.index(matches[0])].createConnection()
        try:
            conn.connect()
            self._atr = bytes(conn.getATR())
        except Exception as exc:  # smartcard.* hierarchy — mapped, not hidden
            raise BackendError(f"connect to {matches[0]} failed: {exc}") from None
        self._conn = conn

    def disconnect(self) -> None:
        if self._conn is not None:
            try:
                self._conn.disconnect()
            except Exception:
                pass
            self._conn = None

    def exchange(self, capdu: bytes) -> bytes:
        if self._conn is None:
            raise BackendError("exchange before connect")
        try:
            data, sw1, sw2 = self._conn.transmit(list(capdu))
        except Exception as exc:
            raise BackendError(f"SCardTransmit failed: {exc}") from None
        return bytes(data) + bytes([sw1, sw2])

    def status(self) -> dict:
        return {"name": self.name, "reader": self.needle,
                "connected": self._conn is not None,
                "atr": self._atr.hex() if self._atr else None}


# --- evidence journal ---------------------------------------------------------


class JsonlJournal:
    """JSONL evidence log. Payloads redacted to len+sha8 unless log_hex."""

    def __init__(self, path, log_hex: bool = False):
        self.path = path
        self.log_hex = log_hex
        self._fh = open(path, "a", encoding="utf-8")

    def log(self, ev: str, **kw) -> None:
        rec = {"ts": round(time.time(), 3), "ev": ev}
        data = kw.pop("data", None)
        if data is not None:
            rec["len"] = len(data)
            rec["sha8"] = hashlib.sha256(data).hexdigest()[:8]
            if self.log_hex:
                rec["hex"] = data.hex()
        rec.update(kw)
        self._fh.write(json.dumps(rec, sort_keys=True) + "\n")
        self._fh.flush()

    def close(self) -> None:
        self._fh.close()


# --- session: one phone socket <-> one backend, strictly sequential ----------


class RelaySession:
    CTRL_DEACTIVATED = b"\x01"
    CTRL_HELLO = b"\x02"

    def __init__(self, sock: socket.socket, backend: CardBackend,
                 journal: Optional[JsonlJournal] = None,
                 delay_ms: int = 0, log_hex: bool = False):
        self.sock = sock
        self.backend = backend
        self.journal = journal
        self.delay_ms = delay_ms
        self.log_hex = log_hex or (journal.log_hex if journal else False)
        self._backend_connected = False

    def _log(self, ev: str, **kw) -> None:
        if self.journal:
            self.journal.log(ev, **kw)

    def _fail(self, message: str) -> int:
        self._log("error", message=message)
        self._teardown()
        return 1

    def _teardown(self) -> None:
        try:
            self.backend.disconnect()
            self._backend_connected = False
        finally:
            try:
                self.sock.close()
            except OSError:
                pass

    def run(self, once: int = 0) -> int:
        self._log("session", backend=self.backend.status(), once=once,
                  delay_ms=self.delay_ms)
        done = 0
        try:
            while once <= 0 or done < once:
                frame = recv_frame(self.sock)
                if frame == self.CTRL_HELLO:
                    self._log("hello")
                    continue
                if frame == self.CTRL_DEACTIVATED:
                    self._log("deactivated")
                    continue
                self._log("rx", data=frame)
                try:
                    if not self._backend_connected:
                        self.backend.connect()
                        self._backend_connected = True
                        self._log("backend-connect", **self.backend.status())
                    if self.delay_ms:
                        time.sleep(self.delay_ms / 1000.0)
                    resp = self.backend.exchange(frame)
                except BackendError as exc:
                    # Explicit failure: no reply is sent for this C-APDU,
                    # the phone socket closes, nothing is retried.
                    return self._fail(str(exc))
                if len(resp) < 2:
                    return self._fail(
                        f"backend {self.backend.name} returned a reply "
                        f"shorter than a status word: {resp.hex()}")
                send_frame(self.sock, resp)
                self._log("tx", data=resp)
                done += 1
        except EOFError:
            self._log("drop", reason="phone closed the relay socket")
            self._teardown()
            return 0
        except (OSError, RelayError) as exc:
            return self._fail(f"relay link: {exc}")
        self._log("done", exchanges=done)
        self._teardown()
        return 0


# --- adb plumbing + CLI -------------------------------------------------------


def adb(*args: str) -> str:
    return subprocess.run(["adb", *args], capture_output=True,
                          text=True, timeout=30).stdout.strip()


def connect(serial: Optional[str]) -> socket.socket:
    base = ["-s", serial] if serial else []
    adb(*base, "forward", f"tcp:{HOST_PORT}", f"localabstract:{SOCKET_NAME}")
    for _ in range(20):
        try:
            s = socket.create_connection(("127.0.0.1", HOST_PORT), timeout=2)
            s.settimeout(None)  # block indefinitely waiting for reader traffic
            return s
        except OSError:
            time.sleep(0.5)
    raise SystemExit("cannot reach the relay socket — is the app "
                     "installed and NFC on?")


def build_backend(args) -> CardBackend:
    miss = bytes.fromhex(args.miss)
    if args.backend == "pcsc":
        if not args.reader:
            raise SystemExit("--backend pcsc needs --reader NEEDLE")
        return PcscBackend(args.reader)
    if args.backend == "static":
        return StaticBackend(bytes.fromhex(args.respond))
    table = load_script(args.script) if args.script else {}
    if args.backend == "script":
        if not args.script:
            raise SystemExit("--backend script needs --script FILE ('-' "
                             "reads stdin)")
        return ScriptBackend(table, miss=miss)
    # auto: script table when given, otherwise the fixed responder
    if table:
        return ScriptBackend(table, miss=miss)
    return StaticBackend(bytes.fromhex(args.respond))


def main() -> int:
    ap = argparse.ArgumentParser(
        description="host card brain for the bench HCE relay (#529)")
    ap.add_argument("--serial", default=None, help="adb device serial")
    ap.add_argument("--backend", choices=["auto", "static", "script", "pcsc"],
                    default="auto",
                    help="where R-APDUs come from (default: script table "
                         "when --script is given, else the fixed responder)")
    ap.add_argument("--respond", default="9000",
                    help="fixed R-APDU hex for the static backend "
                         "(default 9000)")
    ap.add_argument("--script", default=None,
                    help="file of CAPDU_HEX=RAPDU_HEX lines ('-' = stdin)")
    ap.add_argument("--miss", default="6a82",
                    help="script-backend miss SW (default 6a82)")
    ap.add_argument("--reader", default=None,
                    help="pcsc backend: reader-name substring")
    ap.add_argument("--delay-ms", type=int, default=0)
    ap.add_argument("--once", type=int, default=0,
                    help="exit after N exchanges")
    ap.add_argument("--journal", default=None,
                    help="JSONL evidence journal path")
    ap.add_argument("--log-hex", action="store_true",
                    help="include raw APDU hex in the journal (default: "
                         "len+sha8 only — sensitive replies stay redacted)")
    ap.add_argument("--meta", action="append", default=[],
                    metavar="k=v", help="journal session-header metadata "
                    "(repeatable)")
    args = ap.parse_args()

    backend = build_backend(args)
    print(f"backend: {backend.status()}")

    journal = JsonlJournal(args.journal, log_hex=args.log_hex) \
        if args.journal else None
    meta = dict(kv.split("=", 1) for kv in args.meta)
    if journal and meta:
        journal.log("meta", **meta)

    s = connect(args.serial)
    print(f"connected (tcp:{HOST_PORT} -> localabstract:{SOCKET_NAME})")
    session = RelaySession(s, backend, journal=journal,
                           delay_ms=args.delay_ms, log_hex=args.log_hex)
    try:
        rc = session.run(once=args.once)
    except KeyboardInterrupt:
        rc = 0
    finally:
        if journal:
            journal.close()
    return rc


if __name__ == "__main__":
    sys.exit(main())
