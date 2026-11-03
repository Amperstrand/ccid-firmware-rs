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


class PolicyViolation(RelayError):
    """A C-APDU is not allowed to reach a physical card (safety policy).

    Raised BEFORE any transport write; the session treats it as a
    hard error (no reply, socket closed) — a denied command must never
    be silently skipped or answered with a fabricated status word.
    """


class CardPolicy:
    """Default-deny APDU gate in front of a physical card.

    A policy starts UNVERIFIED: only its identity-verification command
    may pass. Once the card's observed identity matches the expected
    one, the profile's read-only allowlist unlocks. Every command is
    checked immediately before transmission — caller-side discipline
    is not sufficient for an autonomous laboratory.
    """

    name = "abstract"

    def __init__(self, expected_id: bytes):
        self.expected_id = expected_id
        self.verified = False

    def check(self, capdu: bytes) -> None:
        """Raise PolicyViolation unless the C-APDU may be transmitted."""
        raise NotImplementedError

    def observe(self, capdu: bytes, rapdu: bytes) -> None:
        """Update chain/identity state from a completed exchange.

        May raise PolicyViolation (identity mismatch is fatal)."""

    def status(self) -> dict:
        return {"policy": self.name, "verified": self.verified,
                "expect": self.expected_id.hex()}


class Ntag424ReadOnlyPolicy(CardPolicy):
    """NTAG424 DNA zero-auth read profile (AGENTS card doctrine; the
    2026-11-03 survey's safe set).

    Pre-verification: the GET_VERSION chain only. Identity = the UID
    carried in the chain's final response part. Verified allowlist:
    NDF app SELECT (exact AID D2760000850101), CC/NDEF file SELECTs,
    READ BINARY, plain GetFileSettings, Read_Sig. 90 AF is a chained
    continuation and is only classifiable WITH an open GET_VERSION
    chain. Forbidden by construction: every AUTH (90 77/90 71), key
    change (90 C4), SDM/file-settings writes (5F), SetConfiguration
    (5C), UpdateBinary/WriteData (D6/8D), counters, proprietary file.
    """

    name = "nt424-read"
    _GV_START = bytes.fromhex("9060000000")
    _NDEF_AID = bytes.fromhex("D2760000850101")

    def __init__(self, expected_id: bytes):
        super().__init__(expected_id)
        self._gv_chain_open = False
        self._dead = False

    def check(self, capdu: bytes) -> None:
        if self._dead:
            raise PolicyViolation(
                "card identity mismatch observed — this policy is dead "
                "for the rest of the session")
        if capdu[:2] == b"\x90\xaf":
            if not self._gv_chain_open:
                raise PolicyViolation(
                    "90 AF continuation without an open GET_VERSION "
                    "chain — unclassifiable, denied")
            return
        self._gv_chain_open = False  # any other command breaks the chain
        if capdu[:5] == self._GV_START:
            return
        # HCE bootstrap exception: the NDEF-app SELECT is the reader's
        # session-BINDING command (the phone dispatches on it) — it is
        # itself a pure read, so it may pass pre-verification. Every
        # other SELECT stays behind the identity gate.
        if self._is_ndef_app_select(capdu):
            return
        if not self.verified:
            raise PolicyViolation(
                "card identity not verified — run the GET_VERSION "
                "chain first (UID must match the expected identity)")
        if capdu[:2] == b"\x00\xa4":  # SELECT: CC/NDEF file only
            p1, p2, lc = capdu[2], capdu[3], capdu[4]
            aid = capdu[5:5 + lc]
            if (p1, p2) == (0x00, 0x0C) and lc == 2 and \
                    aid in (bytes.fromhex("E103"), bytes.fromhex("E104")):
                return
            raise PolicyViolation(
                f"SELECT outside the NTAG424 read profile: {capdu.hex()}")
        if capdu[:2] == b"\x00\xb0":  # READ BINARY (any offset/len)
            return
        if capdu == bytes.fromhex("90F50000010200"):   # GetFileSettings
            return
        if capdu == bytes.fromhex("903C0000010000"):   # Read_Sig
            return
        raise PolicyViolation(
            f"command not in the NTAG424 read-only profile: {capdu.hex()}")

    @classmethod
    def _is_ndef_app_select(cls, capdu: bytes) -> bool:
        if capdu[:2] != b"\x00\xa4" or capdu[2:4] != b"\x04\x00":
            return False
        lc = capdu[4]
        return capdu[5:5 + lc] == cls._NDEF_AID

    def observe(self, capdu: bytes, rapdu: bytes) -> None:
        sw = bytes(rapdu[-2:])
        if capdu[:5] == self._GV_START or capdu[:2] == b"\x90\xaf":
            self._gv_chain_open = (sw == b"\x91\xaf")
            if sw == b"\x91\x00":
                uid = bytes(rapdu[:-2])[:7]
                if uid != self.expected_id:
                    self._dead = True   # poison: even the bootstrap SELECT dies
                    raise PolicyViolation(
                        f"NTAG424 UID mismatch: card={uid.hex()} "
                        f"expected={self.expected_id.hex()}")
                self.verified = True


class JcopReadOnlyPolicy(CardPolicy):
    """JCOP/J3R180 read profile, gated by the CPLC ledger (#127).

    Pre-verification: GET DATA CPLC (80 CA 9F 7F 00) only; the CPLC
    serial (offset 11..14 of the 9F7F body) must match the expected
    ledger identity — ATR alone is NEVER identity. Verified allowlist:
    SELECT by AID / by file id (pure read per the doctrine table) and
    GET DATA with approved read tags (CPLC 9F7F, applet AID 004F).
    Forbidden by construction: INITIALIZE UPDATE / EXTERNAL
    AUTHENTICATE (budget-bearing), VERIFY/CHANGE REFERENCE DATA,
    INSTALL/LOAD/DELETE/GET STATUS (GP management), SC-gated GET DATA
    (00E0), writes, unknown commands.
    """

    name = "jcop-read"
    _CPLC = bytes.fromhex("80CA9F7F00")
    _GD_TAGS = {bytes.fromhex("9F7F"), bytes.fromhex("004F")}

    def check(self, capdu: bytes) -> None:
        if capdu[:5] == self._CPLC:
            return
        if not self.verified:
            raise PolicyViolation(
                "card identity not verified — GET DATA CPLC must match "
                "the expected ledger serial first (ATR is not identity)")
        if capdu[:2] == b"\x00\xa4" and capdu[2] in (0x00, 0x04):
            return  # SELECT by file id / by AID: pure read
        if capdu[:2] in (b"\x80\xca", b"\x00\xca") and \
                capdu[2:4] in self._GD_TAGS:
            return
        raise PolicyViolation(
            f"command not in the JCOP read-only profile: {capdu.hex()}")

    def observe(self, capdu: bytes, rapdu: bytes) -> None:
        if capdu[:5] != self._CPLC or bytes(rapdu[-2:]) != b"\x90\x00":
            return
        body = bytes(rapdu[:-2])
        if body[:2] != bytes.fromhex("9F7F"):
            return
        # Ledger serial position: bytes 15..18 of the tag-included 9F7F
        # body — bench-verified on three P71D321 cards (50942720 /
        # 50947320 / 50979692, #127 ledger, 2026-11-03).
        serial = body[15:19]
        if serial != self.expected_id:
            raise PolicyViolation(
                f"CPLC identity mismatch: card={serial.hex()} "
                f"expected={self.expected_id.hex()}")
        self.verified = True


class PcscBackend(CardBackend):
    """Real card in a PC/SC reader: the live-card relay backend.

    One reader (by name substring), one connection, no retries: any
    card/reader error raises BackendError and the session dies loudly —
    the doctrine forbids re-pairing replies across a failed exchange or
    silently reconnecting to a different card instance.
    """

    name = "pcsc"

    def __init__(self, reader_needle: str, policy: Optional[CardPolicy] = None,
                 unsafe_raw: bool = False):
        if policy is None and not unsafe_raw:
            raise RelayError(
                "PcscBackend refuses to run without a card-safety policy "
                "(default-deny at the physical-card boundary). Pass a "
                "policy from --pcsc-policy/--expect-id, or "
                "unsafe_raw=True explicitly for owner-supervised "
                "experiments.")
        self.needle = reader_needle
        self.policy = policy
        self.unsafe_raw = unsafe_raw
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
        entries = [(str(obj), obj) for obj in readers()]
        matches = [(name, obj) for name, obj in entries
                   if self.needle.lower() in name.lower()]
        if not matches:
            raise BackendError(f"no PC/SC reader matches {self.needle!r}; "
                               f"present: {[name for name, _ in entries]}")
        if len(matches) > 1:
            raise BackendError(f"reader needle {self.needle!r} is ambiguous: "
                               f"{[name for name, _ in matches]}")
        name, obj = matches[0]
        conn = obj.createConnection()
        try:
            conn.connect()
            self._atr = bytes(conn.getATR())
        except Exception as exc:  # smartcard.* hierarchy — mapped, not hidden
            raise BackendError(f"connect to {name} failed: {exc}") from None
        self._conn = conn

    def disconnect(self) -> None:
        if self._conn is not None:
            try:
                self._conn.disconnect()
            except Exception:
                pass
            self._conn = None

    def _transmit_impl(self, capdu: bytes) -> bytes:
        """Physical transport. Overridden by offline tests."""
        try:
            data, sw1, sw2 = self._conn.transmit(list(capdu))
        except Exception as exc:
            raise BackendError(f"SCardTransmit failed: {exc}") from None
        return bytes(data) + bytes([sw1, sw2])

    def exchange(self, capdu: bytes) -> bytes:
        if self._conn is None:
            raise BackendError("exchange before connect")
        if self.policy is not None:
            # The safety boundary: enforced immediately before the
            # physical exchange, independent of the caller's discipline.
            self.policy.check(capdu)
        resp = self._transmit_impl(capdu)
        if self.policy is not None:
            self.policy.observe(capdu, resp)
        return resp

    def status(self) -> dict:
        st = {"name": self.name, "reader": self.needle,
              "connected": self._conn is not None,
              "atr": self._atr.hex() if self._atr else None}
        if self.policy is not None:
            st.update(self.policy.status())
        elif self.unsafe_raw:
            st["policy"] = "raw-unsafe"
        return st


class AndroidReaderBackend(CardBackend):
    """Phone B as the card source: ReaderMode + IsoDep over the
    bench-reader-relay socket (the #529 second-phone leg).

    Direction is REVERSED vs the HCE link: the HOST sends C-APDU
    frames down (adb forward tcp:4872 ->
    localabstract:bench-reader-relay) and the phone answers with the
    physical card's R-APDU. Controls from the phone: 0x01 tag-lost
    (session-invalidating), 0x03 tag-present (info). Strictly
    sequential exchanges; one tag per session; tag-lost is an explicit
    BackendError — never a reconnect to a different tag, never a blind
    retransmit. The same default-deny card-safety policy guards this
    physical-card boundary as the PC/SC one.
    """

    name = "android-reader"
    READER_PORT = 4872
    READER_SOCKET = "bench-reader-relay"
    CTRL_TAG_LOST = b"\x01"
    CTRL_TAG_PRESENT = b"\x03"

    def __init__(self, serial: Optional[str] = None,
                 policy: Optional[CardPolicy] = None,
                 unsafe_raw: bool = False):
        if policy is None and not unsafe_raw:
            raise RelayError(
                "AndroidReaderBackend refuses to run without a "
                "card-safety policy (same boundary as PcscBackend); "
                "pass --pcsc-policy/--expect-id or unsafe_raw=True.")
        self.serial = serial
        self.policy = policy
        self.unsafe_raw = unsafe_raw
        self._sock: Optional[socket.socket] = None
        self._connected = False
        self._tag_info = ""

    def connect(self) -> None:
        base = ["-s", self.serial] if self.serial else []
        adb(*base, "forward", f"tcp:{self.READER_PORT}",
            f"localabstract:{self.READER_SOCKET}")
        for _ in range(20):
            try:
                self._sock = socket.create_connection(
                    ("127.0.0.1", self.READER_PORT), timeout=2)
                self._sock.settimeout(None)
                self._connected = True
                return
            except OSError:
                time.sleep(0.5)
        raise BackendError(
            f"cannot reach {self.READER_SOCKET} on the phone — is the "
            f"ReaderMode companion running with a tag in the field?")

    def disconnect(self) -> None:
        if self._sock is not None:
            try:
                self._sock.close()
            except OSError:
                pass
            self._sock = None
        self._connected = False

    def exchange(self, capdu: bytes) -> bytes:
        if not self._connected or self._sock is None:
            raise BackendError("exchange before connect")
        if self.policy is not None:
            self.policy.check(capdu)
        send_frame(self._sock, capdu)
        while True:
            resp = recv_frame(self._sock)
            if resp == self.CTRL_TAG_LOST:
                self._connected = False
                raise BackendError(
                    "tag lost (phone ReaderMode deactivated) — session "
                    "invalidated, no retransmit")
            if resp[:1] == self.CTRL_TAG_PRESENT:
                self._tag_info = resp[1:].decode(errors="replace")
                continue   # info control, not a card answer
            if self.policy is not None:
                self.policy.observe(capdu, resp)
            return resp

    def status(self) -> dict:
        st = {"name": self.name,
              "connected": self._connected,
              "tag": self._tag_info or None}
        if self.policy is not None:
            st.update(self.policy.status())
        elif self.unsafe_raw:
            st["policy"] = "raw-unsafe"
        return st


class FakeReaderBackend(CardBackend):
    """Offline Phone-B stand-in: a canned tag table, optional
    tag-lost injection. Speaks the same session semantics
    (one tag, sequential, explicit loss) so relay logic can be
    developed and tested with no phone present. Same policy guard:
    a fake boundary must not train unsafe habits."""

    name = "fake-reader"

    def __init__(self, tag: dict, lose_after: int = -1,
                 policy: Optional[CardPolicy] = None,
                 unsafe_raw: bool = False):
        if policy is None and not unsafe_raw:
            raise RelayError(
                "FakeReaderBackend also requires a card-safety policy "
                "(or unsafe_raw=True) — the boundary discipline is "
                "uniform across backends.")
        self.tag = tag
        self.lose_after = lose_after
        self.policy = policy
        self.unsafe_raw = unsafe_raw
        self._exchanges = 0

    def connect(self) -> None:
        self._exchanges = 0

    def exchange(self, capdu: bytes) -> bytes:
        if self.policy is not None:
            self.policy.check(capdu)
        if 0 <= self.lose_after <= self._exchanges:
            raise BackendError("tag lost (simulated)")
        self._exchanges += 1
        resp = self.tag.get(capdu, b"\x6a\x82")
        if self.policy is not None:
            self.policy.observe(capdu, resp)
        return resp

    def status(self) -> dict:
        st = {"name": self.name, "entries": len(self.tag),
              "exchanges": self._exchanges}
        if self.policy is not None:
            st.update(self.policy.status())
        elif self.unsafe_raw:
            st["policy"] = "raw-unsafe"
        return st


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
                 delay_ms: int = 0):
        self.sock = sock
        self.backend = backend
        self.journal = journal
        self.delay_ms = delay_ms
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


def make_policy(args):
    """Build the card-safety policy for the pcsc backend from CLI args."""
    if not getattr(args, "pcsc_policy", None):
        if getattr(args, "unsafe_raw_pcsc", False):
            return None  # explicit owner-supervised opt-out
        raise SystemExit(
            "--backend pcsc requires a card-safety policy: pass "
            "--pcsc-policy {nt424-read,jcop-read} + --expect-id HEX "
            "(or --unsafe-raw-pcsc for owner-supervised experiments)")
    if not getattr(args, "expect_id", None):
        raise SystemExit("--pcsc-policy needs --expect-id "
                         "(NTAG424: 7-byte UID hex; JCOP: 4-byte CPLC "
                         "serial hex from the #127 ledger)")
    try:
        eid = bytes.fromhex(args.expect_id)
    except ValueError as exc:
        raise SystemExit(f"--expect-id: bad hex ({exc})") from None
    if args.pcsc_policy == "nt424-read":
        if len(eid) != 7:
            raise SystemExit("nt424-read expects a 7-byte UID "
                             f"(got {len(eid)} bytes)")
        return Ntag424ReadOnlyPolicy(eid)
    if len(eid) != 4:
        raise SystemExit("jcop-read expects a 4-byte CPLC serial "
                         f"(got {len(eid)} bytes)")
    return JcopReadOnlyPolicy(eid)


def build_backend(args) -> CardBackend:
    miss = bytes.fromhex(args.miss)
    if args.backend == "pcsc":
        if not args.reader:
            raise SystemExit("--backend pcsc needs --reader NEEDLE")
        policy = make_policy(args)
        return PcscBackend(args.reader, policy=policy,
                           unsafe_raw=policy is None)
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
    ap.add_argument("--pcsc-policy", choices=["nt424-read", "jcop-read"],
                    default=None,
                    help="card-safety profile enforced at the physical "
                         "card boundary (required for --backend pcsc)")
    ap.add_argument("--expect-id", default=None,
                    help="expected card identity for --pcsc-policy: "
                         "NTAG424 7-byte UID hex, or JCOP 4-byte CPLC "
                         "serial hex (the #127 ledger)")
    ap.add_argument("--unsafe-raw-pcsc", action="store_true",
                    help="explicitly waive the pcsc card-safety policy "
                         "(owner-supervised experiments only)")
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
                           delay_ms=args.delay_ms)
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
