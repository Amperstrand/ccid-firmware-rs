#!/usr/bin/env python3
"""field_recorder — ACR1252 evidence recorder: NFC tcpdump as JSONL.

Host-side observer tool for the bench ORACLE reader (the ACS ACR1252
PICC interface). It records what the reader/card pair does into an
append-only JSONL file — one event per line — so a bench session can
attach evidence ("this exact ATR / SW / timing was observed through
the reference reader at this time") to an issue, exactly the way a
network debug attaches a pcap. Kanban t_b8b5e0a4.

Bench doctrine encoded here (AGENTS.md):

* Reader pinned by EXACT pcscd name, never enumeration index (default
  DEFAULT_READER, overridable with --reader). Zero or multiple exact
  matches is a loud error carrying the full reader listing.
* Hardware imports (pyscard/smartcard) live INSIDE the PcscHardware
  methods, never at module scope — the CI python-collect gate imports
  this file in a minimal env (pytest + stdlib only).
* Every loop is bounded: connect retries, the APDU set (finite by
  construction), and the presence poll (deadline + --max-events cap).
* Card safety: SC-class APDUs (INITIALIZE UPDATE 80 50, EXTERNAL
  AUTHENTICATE 84 82, SC-gated GET DATA with P1P2 00 E0) are REFUSED
  at both the CLI boundary and the library send boundary — each is a
  budget-bearing authentication event on JCOP-class cards (AGENTS.md
  card-handling doctrine / #127 ISD-lock incident).

EVIDENCE FORMAT — one JSON object per line, append-only:

  {"ts": "2026-10-21T14:03:09.123456+02:00",   # ISO-8601, local tz
   "kind": "<event kind>",                      # see below
   "reader": "<exact pcscd reader name>",       # null for reader_list
   "data": {<kind-specific payload>}}

Event kinds:

  reader_list    data.readers             full pcscd reader listing
  card_atr       data.atr / data.atr_len  connect + ATR capture hex
                 data.error               on connect failure
  apdu_exchange  data.request             APDU sent (hex)
                 data.response            response bytes sans SW (hex)
                 data.sw                  status word "SW1SW2" (hex)
                 data.ms                  round-trip milliseconds
                 data.error               on transport/protocol error
  poll_presence  data.event               "initial" | "insert" | "remove"
                 data.present             bool state after the event
                 data.elapsed_s           seconds since poll start
  recorder_meta  first line: argv, pinned reader, poll/APDU config
  recorder_stop  last line: reason ("completed" | "max-events"),
                 per-kind event counts

poll_presence samples card presence without powering the card
(SCardGetStatusChange with a 0 ms timeout — the pcsc-lite status
snapshot; pyscard returns (reader, eventstate, atr) tuples, the
PRESENT bit is the sample) and records only TRANSITIONS after the
initial sample — tcpdump-style, not a heartbeat log.

Usage:
  python3 field_recorder.py --out evidence.jsonl
  python3 field_recorder.py --out e.jsonl --apdu 00A40400023132 \
      --apdu 00CA006E00 --interval 0.5 --duration 30
  python3 field_recorder.py --out e.jsonl --reader "ACS ACR1252 Dual Reader [ACR1252 Dual Reader PICC] 01 00"

Exit 0 = clean stop (including the --max-events cap, tcpdump -c
semantics); 2 = usage error; 1 = reader/bench error.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from datetime import datetime
from pathlib import Path

# The bench oracle: the ACR1252 PICC (contactless) interface, slot 02
# on the .208 bench (the labgrid README gp pin). Override with --reader
# when the bench slot numbering differs (perflib records slot 01 too).
# Reader-pinning doctrine: EXACT name match, never a substring, never
# an index.
DEFAULT_READER = "ACS ACR1252 Dual Reader [ACR1252 Dual Reader PICC] 02 00"

KIND_READER_LIST = "reader_list"
KIND_CARD_ATR = "card_atr"
KIND_APDU_EXCHANGE = "apdu_exchange"
KIND_POLL_PRESENCE = "poll_presence"
KIND_META = "recorder_meta"
KIND_STOP = "recorder_stop"

# --- SC-class APDU guard (AGENTS.md card-handling doctrine) ----------------
# Copied verbatim-in-spirit from tests/hardware/nfc/gempc_t1.py
# (branch tools/gempc-t1-harness): INITIALIZE UPDATE (80 50), EXTERNAL
# AUTHENTICATE (84 82), SC-gated GET DATA. The GET DATA tag rides
# P1/P2 (CLA/INS = 80 CA or 84 CA, P1P2 = 00 E0) — a prefix check on
# CLA/INS alone misses it; the legacy 00 E0-CLA form is kept for belt
# and braces. Logical-channel CLA bits are normalized first so
# `81 50` / `85 82` on channel 1+ cannot slip past.
SC_CLASS_PREFIXES = (b"\x80\x50", b"\x84\x82", b"\x00\xe0")
GP_GET_DATA_INS = (0xCA,)
GP_GET_DATA_TAGS = (b"\x00\xe0",)


def _strip_channel(cla: int) -> int:
    """Drop the logical-channel bits (b1..b3 for CLA >= 0x80)."""
    return (cla & 0xF8) if cla >= 0x80 else cla


def is_sc_class_apdu(apdu: bytes) -> bool:
    """True for budget-bearing GP secure-channel commands (AGENTS.md
    safe/unsafe table). Such APDUs are never sent by this tool."""
    if len(apdu) < 2:
        return False
    if bytes([_strip_channel(apdu[0]), apdu[1]]) in (
            bytes([_strip_channel(p[0]), p[1]]) for p in SC_CLASS_PREFIXES):
        return True
    if (len(apdu) >= 4 and apdu[1] in GP_GET_DATA_INS
            and apdu[2:4] in GP_GET_DATA_TAGS
            and _strip_channel(apdu[0]) in (0x80, 0x84)):
        return True
    return False


class RecorderError(RuntimeError):
    """Reader/bench-level failure; str() names the failed stage."""


class NoCardError(RecorderError):
    """No card came up on connect (translated from smartcard's
    NoCardException / CardConnectionException at the hw seam so the
    recorder core — and the unit-test fakes — never need pyscard)."""


class EventCapReached(RecorderError):
    """--max-events hit: stop cleanly, tcpdump -c semantics."""


# ---------------------------------------------------------------------------
# Event sink — append-only JSONL, flushed per line so a killed run still
# leaves its evidence (the j3r180 LiveLog lesson: print-at-exit lost an
# 800 s run's whole log).
# ---------------------------------------------------------------------------

class JsonlWriter:
    def __init__(self, path: str | Path, max_events: int = 1000):
        if max_events < 1:
            raise ValueError("max_events must be >= 1")
        self.path = Path(path)
        self.max_events = max_events
        self.count = 0
        self.counts: dict[str, int] = {}
        self._fh = None  # opened lazily: first event creates/appends

    def _ensure_open(self):
        if self._fh is None:
            self._fh = self.path.open("a", encoding="utf-8")
        return self._fh

    def write(self, kind: str, reader: str | None, data: dict) -> None:
        """Append one event; raises EventCapReached past the cap."""
        if self.count >= self.max_events:
            raise EventCapReached(
                f"--max-events {self.max_events} reached")
        self._append(kind, reader, data)

    def write_final(self, kind: str, reader: str | None, data: dict) -> None:
        """Terminal marker (recorder_stop) — bypasses the cap so the
        evidence file always ends with an explicit stop line."""
        self._append(kind, reader, data)

    def _append(self, kind: str, reader: str | None, data: dict) -> None:
        event = {
            "ts": datetime.now().astimezone().isoformat(),
            "kind": kind,
            "reader": reader,
            "data": data,
        }
        fh = self._ensure_open()
        fh.write(json.dumps(event) + "\n")
        fh.flush()
        self.count += 1
        self.counts[kind] = self.counts.get(kind, 0) + 1

    def close(self) -> None:
        if self._fh is not None:
            self._fh.close()
            self._fh = None


# ---------------------------------------------------------------------------
# Hardware adapter — ALL smartcard/pyscard imports live inside these
# methods (CI python-collect gate: module scope stays stdlib-only).
# Tests duck-type this class; only PcscHardware itself touches pcscd.
# ---------------------------------------------------------------------------

class PcscCard:
    """One connected card through pcscd: ATR + transmit + close.

    This is the duck type unit tests fake: `.atr` bytes, `.transmit()
    -> (response-sans-SW, SW int)`, `.close()`."""

    def __init__(self, conn, atr: bytes):
        self._conn = conn
        self._atr = bytes(atr)

    @property
    def atr(self) -> bytes:
        return self._atr

    def transmit(self, apdu: bytes) -> tuple[bytes, int]:
        data, sw1, sw2 = self._conn.transmit(list(apdu))
        return bytes(data), (sw1 << 8) | sw2

    def close(self) -> None:
        try:
            self._conn.disconnect()
        except Exception:
            pass


class PcscHardware:
    """pcscd-backed implementation of the recorder's hardware seam."""

    def list_readers(self) -> list[str]:
        from smartcard.System import readers
        return [str(r) for r in readers()]

    def connect(self, reader: str) -> PcscCard:
        """Connect (power on) the card in the exact-named reader.
        Raises NoCardError (translated from smartcard's
        NoCardException / CardConnectionException) as the failure
        evidence the caller records."""
        from smartcard.Exceptions import (CardConnectionException,
                                          NoCardException)
        from smartcard.pcsc.PCSCCardConnection import PCSCCardConnection
        conn = PCSCCardConnection(reader)
        try:
            conn.connect()
            atr = bytes(conn.getATR())
        except (NoCardException, CardConnectionException) as exc:
            try:
                conn.disconnect()
            except Exception:
                pass
            raise NoCardError(f"{type(exc).__name__}: {exc}") from exc
        return PcscCard(conn, atr)

    def present(self, reader: str) -> bool:
        """Card-present snapshot WITHOUT powering the card.

        pcsc-lite status poll: SCardGetStatusChange with a 0 ms
        timeout and current-state UNAWARE returns the reader's state
        immediately (pyscard contract: (readername, eventstate, atr)
        tuples; the PRESENT bit says a card is in the field)."""
        from smartcard.scard import (SCARD_S_SUCCESS, SCARD_SCOPE_USER,
                                     SCARD_STATE_PRESENT,
                                     SCARD_STATE_UNAWARE, SCardEstablishContext,
                                     SCardGetStatusChange,
                                     SCardReleaseContext)
        hresult, hcontext = SCardEstablishContext(SCARD_SCOPE_USER)
        if hresult != SCARD_S_SUCCESS:
            raise RecorderError(f"SCardEstablishContext failed: {hresult:#x}")
        try:
            states = [(reader, SCARD_STATE_UNAWARE)]
            hresult, newstates = SCardGetStatusChange(hcontext, 0, states)
            if hresult != SCARD_S_SUCCESS:
                raise RecorderError(
                    f"SCardGetStatusChange failed: {hresult:#x} "
                    f"for '{reader}'")
            _reader, eventstate, _atr = newstates[0]
            return bool(eventstate & SCARD_STATE_PRESENT)
        finally:
            SCardReleaseContext(hcontext)


def pin_reader(names: list[str], wanted: str) -> str:
    """Exact-name reader pinning (bench doctrine rule 2).

    Zero or multiple exact matches raise RecorderError with the full
    listing — substring matches are deliberately not accepted (a
    'contains' pin would silently follow reader re-enumeration)."""
    matches = [n for n in names if n == wanted]
    if len(matches) == 1:
        return matches[0]
    listing = "\n".join(f"  {n}" for n in names) or "  (pcscd lists no readers)"
    raise RecorderError(
        f"pinned reader not resolvable: wanted exactly '{wanted}', "
        f"got {len(matches)} exact matches. pcscd lists:\n{listing}")


def _stop_data(reason: str, writer: JsonlWriter) -> dict:
    """Payload for the terminal recorder_stop marker — self-consistent:
    counts (including the marker itself) sum to `events`, which equals
    the total line count of the evidence file."""
    counts = dict(writer.counts)
    counts[KIND_STOP] = counts.get(KIND_STOP, 0) + 1
    return {"reason": reason, "events": writer.count + 1,
            "counts": counts}


# ---------------------------------------------------------------------------
# The recorder itself — pure orchestration over the hw seam + writer.
# ---------------------------------------------------------------------------

class FieldRecorder:
    def __init__(self, hw, writer: JsonlWriter, reader: str,
                 sleep_fn=time.sleep, monotonic_fn=time.monotonic,
                 timer_fn=time.perf_counter):
        self.hw = hw
        self.writer = writer
        self.reader = reader
        self._sleep = sleep_fn
        self._monotonic = monotonic_fn
        self._timer = timer_fn

    # -- stage: reader_list -------------------------------------------------
    def record_reader_list(self) -> list[str]:
        names = self.hw.list_readers()
        self.writer.write(KIND_READER_LIST, None, {"readers": names})
        return names

    # -- stage: card_atr ------------------------------------------------------
    def capture_atr(self, attempts: int = 3, delay: float = 1.0):
        """Connect (bounded retries) and record the ATR.

        Returns the card handle (transmit-ready) or None when no card
        came up across the attempts — an error event is recorded either
        way, the evidence file tells the story."""
        last_error = "no attempt made"
        for attempt in range(1, attempts + 1):
            try:
                card = self.hw.connect(self.reader)
            except NoCardError as exc:
                last_error = str(exc)
                if attempt < attempts:
                    self._sleep(delay)
                continue
            self.writer.write(KIND_CARD_ATR, self.reader,
                              {"atr": card.atr.hex(), "atr_len": len(card.atr)})
            return card
        self.writer.write(KIND_CARD_ATR, self.reader, {"error": last_error})
        return None

    # -- stage: apdu_exchange --------------------------------------------------
    def exchange_apdus(self, card, apdus: list[bytes]) -> None:
        """Send the scripted APDU set; one event per APDU.

        The SC-class guard fires HERE, at the library send boundary —
        not only in the CLI — so no caller can loop INITIALIZE UPDATE
        / EXTERNAL AUTHENTICATE / guarded GET DATA through this API
        (the gempc_t1 Codex-P1 lesson)."""
        for apdu in apdus:
            if is_sc_class_apdu(apdu):
                raise RecorderError(
                    f"refusing SC-class APDU {apdu.hex()} (card-safety "
                    "doctrine, AGENTS.md / #127 ISD-lock incident)")
            t0 = self._timer()
            try:
                resp, sw = card.transmit(apdu)
            except Exception as exc:  # transport errors are evidence
                self.writer.write(
                    KIND_APDU_EXCHANGE, self.reader,
                    {"request": apdu.hex(),
                     "error": f"{type(exc).__name__}: {exc}"})
                continue
            ms = (self._timer() - t0) * 1000.0
            self.writer.write(
                KIND_APDU_EXCHANGE, self.reader,
                {"request": apdu.hex(), "response": resp.hex(),
                 "sw": f"{sw:04X}", "ms": round(ms, 3)})

    # -- stage: poll_presence --------------------------------------------------
    def poll_presence(self, interval: float, duration: float) -> None:
        """Bounded periodic presence sampling; transition events only.

        The first sample records the initial condition (event
        "initial"); after that only changes are recorded as
        "insert"/"remove". Bounded by BOTH the duration deadline and
        the writer's --max-events cap."""
        if interval <= 0:
            raise ValueError("interval must be > 0")
        deadline = self._monotonic() + duration
        start = self._monotonic()
        last: bool | None = None
        while True:
            now = self._monotonic()
            if now >= deadline:
                return
            present = self.hw.present(self.reader)
            if last is None or present != last:
                event = ("initial" if last is None
                         else ("insert" if present else "remove"))
                self.writer.write(
                    KIND_POLL_PRESENCE, self.reader,
                    {"event": event, "present": present,
                     "elapsed_s": round(now - start, 3)})
                last = present
            remaining = deadline - self._monotonic()
            if remaining <= 0:
                return
            self._sleep(min(interval, remaining))

    # -- full session --------------------------------------------------------
    def run(self, apdus: list[bytes], interval: float, duration: float,
            connect_attempts: int = 3) -> str:
        """CLI session: reader_list -> pin -> ATR -> APDUs -> poll.

        Returns the stop reason ("completed" or "max-events")."""
        self.writer.write(KIND_META, self.reader,
                          {"tool": "field_recorder.py", "argv": sys.argv,
                           "interval": interval, "duration": duration,
                           "apdus": [a.hex() for a in apdus],
                           "max_events": self.writer.max_events})
        try:
            names = self.record_reader_list()
            pin_reader(names, self.reader)
            card = self.capture_atr(attempts=connect_attempts)
            if card is not None and apdus:
                self.exchange_apdus(card, apdus)
            if card is not None:
                card.close()
            if duration > 0:
                self.poll_presence(interval, duration)
            reason = "completed"
        except EventCapReached:
            reason = "max-events"
        self.writer.write_final(KIND_STOP, self.reader,
                                _stop_data(reason, self.writer))
        return reason


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def parse_apdu_arg(text: str) -> bytes:
    try:
        apdu = bytes.fromhex(text)
    except ValueError:
        raise argparse.ArgumentTypeError(
            f"not a hex APDU: {text!r}") from None
    if len(apdu) < 4:
        raise argparse.ArgumentTypeError(
            f"APDU shorter than the 4-byte header: {text!r}")
    if is_sc_class_apdu(apdu):
        raise argparse.ArgumentTypeError(
            f"SC-class APDU refused (INIT_UPDATE/EXT_AUTH/SC-gated GET "
            f"DATA burn auth budget — AGENTS.md): {text}")
    return apdu


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(
        description="ACR1252 NFC evidence recorder — append-only JSONL "
                    "(see module docstring for the event schema)")
    ap.add_argument("--out", required=True, metavar="PATH",
                    help="append-only JSONL evidence file")
    ap.add_argument("--reader", default=DEFAULT_READER,
                    help="exact pcscd reader name "
                         "(default: the bench oracle ACR1252 PICC slot)")
    ap.add_argument("--apdu", action="append", default=[], metavar="HEX",
                    type=parse_apdu_arg,
                    help="scripted APDU to exchange after ATR capture "
                         "(repeatable; SC-class APDUs are refused)")
    ap.add_argument("--interval", type=float, default=0.5, metavar="SECS",
                    help="presence poll interval (default 0.5)")
    ap.add_argument("--duration", type=float, default=10.0, metavar="SECS",
                    help="presence poll duration, 0 disables (default 10)")
    ap.add_argument("--max-events", type=int, default=1000, metavar="N",
                    help="safety cap on recorded events (default 1000)")
    return ap


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.interval <= 0:
        build_parser().error("--interval must be > 0")
    if args.duration < 0:
        build_parser().error("--duration must be >= 0")
    writer = JsonlWriter(args.out, max_events=args.max_events)
    recorder = FieldRecorder(PcscHardware(), writer, args.reader)
    try:
        reason = recorder.run(apdus=args.apdu, interval=args.interval,
                              duration=args.duration)
    except RecorderError as exc:
        # bench-level failure AFTER the cap check: still end the file
        # with an explicit stop marker so partial evidence stays usable
        writer.write_final(KIND_STOP, recorder.reader,
                           _stop_data(f"error: {exc}", writer))
        print(f"field_recorder: {exc}", file=sys.stderr)
        return 1
    finally:
        writer.close()
    print(f"field_recorder: {reason} — {writer.count} events -> "
          f"{writer.path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
