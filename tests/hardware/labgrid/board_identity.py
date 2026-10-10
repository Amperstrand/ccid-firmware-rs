#!/usr/bin/env python3
"""Per-board identity probes (#65): the AGENTS.md known-good matrix as code.

A place you don't hold is a board you don't own — every probe takes the
place (and the bench flock when it drives pcscd or a serial port) first.
"""

from __future__ import annotations

import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from serial_ports import resolve  # noqa: E402

GEMPC_VERSION_STRING = b"GemPC Twin ESP32 1.0"

SYNC = 0x03
CTRL_ACK = 0x06


def _lrc(data: bytes) -> int:
    x = 0
    for b in data:
        x ^= b
    return x


def escape_frame(payload: bytes, seq: int = 1) -> bytes:
    header = bytes([0x6B, len(payload), 0, 0, 0, 0, seq, 0, 0, 0])
    body = bytes([SYNC, CTRL_ACK]) + header + payload
    return body + bytes([_lrc(body)])


def parse_frames(buf: bytes) -> list[tuple[int, bytes]]:
    out = []
    i = 0
    while i < len(buf) - 12:
        if buf[i] != SYNC:
            i += 1
            continue
        f = buf[i:]
        mt = f[2]
        dlen = int.from_bytes(f[3:7], "little")
        total = 13 + dlen
        if total > len(f) or _lrc(f[:total - 1]) != f[total - 1]:
            i += 1
            continue
        out.append((mt, f[12:12 + dlen]))
        i += total
    return out


def serial_exchange(port: str, raw: bytes, settle: float = 0.15,
                    budget: float = 1.5) -> list[tuple[int, bytes]]:
    """One raw write + drain-until-idle read (two empty reads). Requires
    pcscd stopped (it holds the port)."""
    import serial as pyserial

    s = pyserial.Serial(port, 115200, timeout=0.15)
    try:
        s.reset_input_buffer()
        s.write(raw)
        s.timeout = 0.15
        buf = b""
        idle = 0
        t0 = time.time()
        while time.time() - t0 < budget and idle < 2:
            chunk = s.read(8192)
            if chunk:
                buf += chunk
                idle = 0
            else:
                idle += 1
        return parse_frames(buf)
    finally:
        s.close()


def probe_m5stick() -> dict:
    """Escape 0x02 over the GemPC wire → firmware version string; the
    m5stick's ON-DEMAND identity (never an unsolicited FWID: libccidtwin
    is strict about unsolicited bytes on UART0)."""
    port = resolve("m5stick")
    frames = [f for f in serial_exchange(port, escape_frame(b"\x02")) if f[0] >= 0x80]
    ok = bool(frames) and frames[0][0] == 0x83 and GEMPC_VERSION_STRING in frames[0][1]
    return {"ok": ok, "detail": frames[0][1][:40] if frames else "no response"}


def probe_nucula() -> dict:
    """Escape 0x02 over the USB-CDC GemPC wire (same firmware identity)."""
    port = resolve("nucula")
    frames = [f for f in serial_exchange(port, escape_frame(b"\x02")) if f[0] >= 0x80]
    ok = bool(frames) and frames[0][0] == 0x83 and len(frames[0][1]) > 0
    return {"ok": ok, "detail": frames[0][1][:40] if frames else "no response"}


def pcsc_reader_matches(match: str) -> list[str]:
    """All pcscd reader names containing `match` (identity, never
    enumeration order)."""
    r = subprocess.run(
        [sys.executable, "-c",
         "from smartcard.System import readers; "
         f"print('|'.join(str(x) for x in readers() if {match!r} in str(x)))"],
        capture_output=True, text=True, timeout=30,
    )
    return [name for name in r.stdout.strip().split("|") if name]


def probe_pcsc_reader(match: str) -> dict:
    """pcscd lists a reader whose name contains `match` (identity, never
    enumeration order)."""
    matches = pcsc_reader_matches(match)
    return {"ok": bool(matches), "detail": matches[0][:80] if matches else "absent"}


# The AGENTS.md known-good matrix, one entry per bench place.
IDENTITY_PROBES = {
    "stm32-ccid": lambda: probe_pcsc_reader("ST2XXX-001"),
    "nucula-c3": lambda: probe_nucula(),
    "m5stick": lambda: probe_m5stick(),
    "ref-acr1252": lambda: probe_pcsc_reader("ACR1252"),
    "ref-cardman": lambda: probe_pcsc_reader("CardMan"),
}
