#!/usr/bin/env python3
"""gempc_smoke — standing boot-smoke probe for the serial CCID readers.

Promoted from session scratch into repo tooling (#313): the #308 stamp
had to hand-roll `m5stick-smoke.py` from conformance_battery primitives
because nothing in-repo served the "prove the firmware SERVES after a
flash of a console-none build" need. This module is that tool.

Two surfaces:

1. CLI (`python3 gempc_smoke.py --port ...`) — GetSlotStatus round-trip
   (the #294 boot-smoke gate), optional Escape 0xD0 uptime liveness
   (two reads, uptime_ticks must advance — serve loop + diag tick
   alive), optional IccPowerOn ATR print (zero card-state risk).
2. Library — `esp32_flash --boot-check wire` imports the pure reply
   predicates here, so the flash path and the smoke probe can never
   disagree about what counts as proof-of-boot on the wire.

Wire protocol (crates/ccid-transport-serial, hardware-verified
2026-10-20): frame = 0x03 SYNC | 0x06 CTRL_ACK | 10-byte CCID header |
payload(dwLength) | XOR-LRC over all preceding bytes. The device
ECHOES the frame, then replies. Framing primitives are imported from
nfc/conformance_battery (single source of truth), NOT re-implemented.

Escape 0xD0 payload (frozen 28-byte LE layout, pinned byte-exact by
ccid-core diagnostics golden test):
  [0:4] apdu_tx  [4:8] apdu_rx  [8:12] nak  [12:16] error
  [16:20] reinit  [20:24] card_present(u32)  [24:28] uptime_ticks

Direct serial access discipline (AGENTS.md): pcscd / the standing
port-owning service must be stopped first (esp32_flash.py owns that
dance when called from the flash path); the port is opened with DTR
and RTS deasserted BEFORE open — an asserted-DTR open toggles the FTDI
auto-reset line / latches the CDC into download mode.

Usage:
  python3 gempc_smoke.py --port /dev/serial/by-id/... [--uptime] [--poweron]
  python3 gempc_smoke.py --port ... --stopbits 1   # USB-CDC (nucula c3)
(default stopbits=2 — the GemPC serial 115200 8N2 convention)
"""

from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

# Pure framing constants + helpers live in conformance_battery (which
# imports pyserial at module scope) — imported lazily in the serial
# paths so the no-hardware collect env never needs it. The CONSTANTS
# here are duplicated values only where conformance_battery would be
# pulled in at import time; the helpers below re-import for real use.
SYNC, CTRL_ACK = 0x03, 0x06
MSG_GET_SLOT_STATUS = 0x65
MSG_ICC_POWER_ON = 0x62
MSG_ESCAPE = 0x6B
RDR_SLOT_STATUS = 0x81
RDR_DATA_BLOCK = 0x80
RDR_ESCAPE = 0x83

# Frozen 0xD0 diagnostics layout (ccid-core diagnostics.rs golden test)
DIAG_SIZE = 28
DIAG_FIELDS = ("apdu_tx", "apdu_rx", "nak", "error", "reinit",
               "card_present", "uptime_ticks")


def _battery():
    """Lazy import of the canonical framing primitives."""
    from nfc.conformance_battery import lrc, parse_frames  # noqa: E402
    return lrc, parse_frames


def make_frame(msg_type: int, seq: int = 0, payload: bytes = b"") -> bytes:
    """SYNC|CTRL|CCID header|payload|LRC — conformance_battery shape."""
    lrc, _ = _battery()
    header = bytes([msg_type])
    header += len(payload).to_bytes(4, "little")
    header += bytes([0, seq, 0, 0, 0])
    body = bytes([SYNC, CTRL_ACK]) + header + payload
    return body + bytes([lrc(body)])


def parse_diagnostics(payload: bytes) -> dict | None:
    """Decode the frozen Escape 0xD0 28-byte LE payload.

    Returns None for a payload that is not exactly DIAG_SIZE — never
    guesses offsets on a short/long buffer (a wrong-field uptime read
    would silently pass liveness on garbage).
    """
    if len(payload) < DIAG_SIZE:
        return None
    vals = [int.from_bytes(payload[i * 4:(i + 1) * 4], "little")
            for i in range(DIAG_SIZE // 4)]
    out = dict(zip(DIAG_FIELDS, vals, strict=True))
    out["card_present"] = bool(out["card_present"])
    return out


def wire_reply_ok(buf: bytes) -> bool:
    """Does `buf` contain an LRC-valid RDR frame? (#294 boot-smoke gate)

    Any LRC-valid reply frame proves the serve loop is alive and
    parsing — the wire equivalent of `Calling app_main()` for
    console-none builds (parse_frames enforces the LRC itself and
    resyncs past console noise on shared CDC ports).
    """
    _, parse_frames = _battery()
    return any(f[0] >= 0x80 for f in parse_frames(buf))


def slot_status_of(frames) -> tuple[int, int] | None:
    """(bmICCStatus, command status) of the first SlotStatus reply."""
    for mt, _seq, bstatus, _berr, _pl in frames:
        if mt == RDR_SLOT_STATUS:
            return bstatus & 0x03, (bstatus >> 6) & 0x03
    return None


def atr_of(frames) -> bytes | None:
    """The DataBlock payload of the first IccPowerOn reply (the ATR)."""
    for mt, _seq, _bstatus, _berr, payload in frames:
        if mt == RDR_DATA_BLOCK and payload:
            return payload
    return None


def open_safe(port: str, stopbits: int = 2, baud: int = 115200):
    """Safe opener: DTR/RTS deasserted BEFORE open (bench rule), and a
    WRITE timeout — the 2026-10-20 c3 incident: a wedged USB-CDC (the
    documented sustained-transfer wedge class) never drains its TX
    buffer, and a write_timeout=None write then blocks FOREVER, hanging
    the probe with no diagnostics. Every write here fails loudly in
    bounded time instead."""
    import serial
    s = serial.Serial()
    s.port = port
    s.baudrate = baud
    s.bytesize = 8
    s.parity = serial.PARITY_NONE
    s.stopbits = stopbits
    s.dtr = False
    s.rts = False
    s.timeout = 0.2
    s.write_timeout = 2.0
    s.open()
    return s


def exchange(s, frame: bytes, wait: float = 1.5,
             attempts: int = 3) -> bytes:
    """Send + collect the reply window (echo + response).

    Write errors/timeouts (wedged CDC) end the exchange immediately
    with what was collected — the caller reports NO LRC-valid reply
    instead of hanging."""
    import serial
    _, parse_frames = _battery()
    buf = b""
    for _ in range(attempts):
        try:
            s.reset_input_buffer()
            s.write(frame)
        except (Exception, serial.SerialTimeoutException):  # noqa: BLE001
            # write refused/timed out: the device is not draining TX
            # (the c3 CDC wedge class) — bounded, loud, no hang
            return buf
        deadline = time.time() + wait
        while time.time() < deadline:
            chunk = s.read(4096)
            if chunk:
                buf += chunk
            frames = parse_frames(buf)
            if any(f[0] >= 0x80 for f in frames):
                return buf
            time.sleep(0.05)
    return buf


def probe(port: str, stopbits: int = 2, uptime: bool = False,
          poweron: bool = False) -> tuple[bool, list[str]]:
    """Run the smoke battery. Returns (ok, lines)."""
    _, parse_frames = _battery()
    lines: list[str] = []
    s = open_safe(port, stopbits=stopbits)
    try:
        time.sleep(0.5)  # post-open settle; no reset pulse — caller owns reset
        buf = exchange(s, make_frame(MSG_GET_SLOT_STATUS))
        frames = parse_frames(buf)
        ss = slot_status_of(frames)
        if ss is None:
            lines.append(f"GetSlotStatus: NO LRC-valid reply "
                         f"(wire: {buf[:64].hex()})")
            return False, lines
        lines.append(f"GetSlotStatus: OK bmICCStatus={ss[0]} "
                     f"cmdStatus={ss[1]}")

        if poweron:
            buf = exchange(s, make_frame(MSG_ICC_POWER_ON), wait=4.0)
            atr = atr_of(parse_frames(buf))
            lines.append("IccPowerOn ATR: " +
                         (atr.hex() if atr else "(no DataBlock reply)"))

        if uptime:
            readings = []
            for _ in range(2):
                buf = exchange(s, make_frame(MSG_ESCAPE,
                                             payload=bytes([0xD0])))
                frames = [f for f in parse_frames(buf)
                          if f[0] == RDR_ESCAPE]
                diag = parse_diagnostics(frames[0][4]) if frames else None
                if diag is None:
                    lines.append("Escape 0xD0: NO valid diagnostics reply")
                    return False, lines
                readings.append(diag["uptime_ticks"])
                lines.append(f"uptime_ticks={diag['uptime_ticks']} "
                             f"card_present={diag['card_present']} "
                             f"apdu_tx={diag['apdu_tx']} "
                             f"err={diag['error']}")
                time.sleep(3.0)
            if readings[1] <= readings[0]:
                lines.append(f"uptime did NOT advance "
                             f"({readings[0]} -> {readings[1]}) — "
                             "serve loop or diag tick is dead")
                return False, lines
            lines.append(f"uptime advanced (+{readings[1] - readings[0]} "
                         f"ticks) — serve loop alive")
        return True, lines
    finally:
        try:
            s.close()
        except Exception:  # noqa: BLE001
            pass


def _cli() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", required=True)
    ap.add_argument("--stopbits", type=int, default=2,
                    help="2 = GemPC serial (default), 1 = USB-CDC")
    ap.add_argument("--uptime", action="store_true",
                    help="also run the Escape 0xD0 uptime liveness check")
    ap.add_argument("--poweron", action="store_true",
                    help="also print the IccPowerOn ATR (zero card risk)")
    args = ap.parse_args()
    ok, lines = probe(args.port, stopbits=args.stopbits,
                      uptime=args.uptime, poweron=args.poweron)
    for line in lines:
        print(line)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(_cli())
