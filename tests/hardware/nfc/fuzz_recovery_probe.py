#!/usr/bin/env python3
"""Issue #89: map post-malformed-input recovery latency on the bench readers.

The conformance battery flagged exactly one case on the m5stick (1/87): the
truncated-header frame `03 06 65 01` followed by a valid GetSlotStatus got
no response within the probe window. This probe answers the issue's open
question — real parser stall or battery timing — by measuring, for every
fixed fuzz case, how long the reader actually needs to answer a valid
GetSlotStatus afterwards.

Method (issue #89 step 1): send the malformed bytes, then poll
GetSlotStatus every 200 ms for 5 s; recovery latency is the time from the
malformed write to the first valid RDR_to_PC_SlotStatus. Also captures
whether unparseable junk preceded that response on the wire (issue step 3
evidence: echo/frame accumulation across the parser reset on the flashed
bench build — valid echo frames ahead of the response are NORMAL GemPC
behavior and do not count).

Verdict rule (issue step 2): any recovery latency > 1.5 s (the battery's
hard read cap) is a real stall; recoveries inside the cap mean the
battery's idle-terminated read dropped the late response (a false
positive of the pre-settle-fix exchange()).

One serial connection per reader for the whole run: the nucula's
USB-Serial/JTAG CDC dislikes rapid open/close cycles (readiness-without-
data errors — AGENTS.md nucula failure modes).

Direct serial access: pcscd must be stopped. The script manages it —
and acquires the DUT labgrid places first (nucula-c3, m5stick) so it
cannot race concurrent HIL/bolty/micronuts sessions (issue #338).

Usage:
    python3 tests/hardware/nfc/fuzz_recovery_probe.py [--iters 25] [--quick]
"""

import argparse
import random
import statistics
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from conformance_battery import (  # noqa: E402
    CTRL_ACK,
    LABGRID_PLACES,
    MSG_GET_SLOT_STATUS,
    SYNC,
    frame,
    labgrid_acquire,
    labgrid_release,
    lrc,
    pcscd_start,
    pcscd_stop,
)
from serial_ports import resolve  # noqa: E402

import serial  # noqa: E402

POLL_INTERVAL_S = 0.2
RECOVERY_WINDOW_S = 5.0
BATTERY_HARD_CAP_S = 1.5  # exchange() read cap — latencies above are stalls


def fixed_cases() -> list[tuple[str, bytes]]:
    rng = random.Random(20261009)
    return [
        ("bad LRC", frame(MSG_GET_SLOT_STATUS, 1, lrc_override=0xFF)),
        ("truncated header", bytes([SYNC, CTRL_ACK, 0x65, 0x01])),
        ("no SYNC garbage", bytes(rng.randbytes(24))),
        ("SYNC + garbage", bytes([SYNC]) + bytes(rng.randbytes(20))),
        ("oversized dwLength", bytes([SYNC, CTRL_ACK, 0x6F, 0xFF, 0xFF, 0xFF, 0x00,
                                      0x00, 0x01, 0x00, 0x00, 0x00]) + bytes([0x00])),
        ("lone SYNC", bytes([SYNC])),
        ("NAK ctrl frame", bytes([SYNC, 0x15])),
    ]


def find_response(buf: bytes, seq: int):
    """(resp_frame, junk_bytes_before) for the SlotStatus matching seq.

    junk = bytes ahead of the response that do not fully cover as valid
    SYNC-anchored frames (echo frames are valid coverage; accumulated
    garbage or half-frames are junk).
    """
    spans = []  # (start, end) of every valid frame in buf
    i = 0
    while i < len(buf) - 12:
        if buf[i] != SYNC:
            i += 1
            continue
        f = buf[i:]
        dlen = int.from_bytes(f[3:7], "little")
        total = 13 + dlen
        if total > len(f):
            i += 1
            continue
        if lrc(f[: total - 1]) != f[total - 1]:
            i += 1
            continue
        spans.append((i, i + total))
        if f[2] == 0x81 and f[8] == seq:
            resp_end = i + total
            covered = 0
            junk = 0
            for s, e in spans:
                if s > covered:
                    junk += s - covered
                covered = max(covered, e)
            junk += len(buf[covered:resp_end])
            return f[:total], junk
        i += total
    return None, len(buf)


class Port:
    """Persistent serial connection with one CDC-recovery reopen."""

    @staticmethod
    def _open(port: str):
        """Safe opener — the conformance battery's Reader pattern.

        DTR-latch rule (AGENTS.md nucula failure modes; Codex P1 on
        #107, issue #338): pyserial asserts DTR/RTS at open; on the
        nucula CDC that latches boot:0x5 ROM download mode on the next
        reset. Deassert both lines BEFORE open (and again after — a PTY
        surface fails the ioctls with ENOTTY, tolerated per #305).
        8N2 per the GemPC-Twin framing sibling fix: at the 8N1 default
        the m5stick UART sees framing errors and answers nothing
        (battery Reader comment, found 2026-10-20 while instrumenting
        #330) — fix-all-siblings applied here.
        """
        s = serial.Serial(port=None, baudrate=115200,
                          timeout=POLL_INTERVAL_S, stopbits=2)
        s.port = port
        s.dtr = False
        s.rts = False
        s.open()
        try:
            s.setDTR(False)
            s.setRTS(False)
        except OSError:
            pass
        return s

    def __init__(self, name: str):
        self.name = name
        self.s = self._open(resolve(name))

    def reopen(self):
        try:
            self.s.close()
        except Exception:
            pass
        time.sleep(0.5)
        self.s = self._open(resolve(self.name))
        self.s.reset_input_buffer()

    def recovery_latency(self, case: bytes) -> tuple[float | None, int]:
        """Send malformed bytes, poll until valid answer; (latency, junk_bytes)."""
        self.s.reset_input_buffer()
        t0 = time.time()
        self.s.write(case)
        seq = 0
        while time.time() - t0 < RECOVERY_WINDOW_S:
            seq = (seq % 255) + 1
            self.s.write(frame(MSG_GET_SLOT_STATUS, seq))
            buf = b""
            deadline = time.time() + POLL_INTERVAL_S * 2
            while time.time() < deadline:
                buf += self.s.read(8192)
            resp, junk = find_response(buf, seq)
            if resp is not None:
                return time.time() - t0, junk
        return None, 0


def pct(values: list[float], q: float) -> float:
    ordered = sorted(values)
    idx = min(len(ordered) - 1, int(q * len(ordered)))
    return ordered[idx]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--iters", type=int, default=25,
                    help="iterations per case (default 25)")
    ap.add_argument("--quick", action="store_true", help="only the flagged case")
    args = ap.parse_args()

    cases = fixed_cases()
    if args.quick:
        cases = [c for c in cases if c[0] == "truncated header"]

    print(f"iters/case: {args.iters}, poll every {POLL_INTERVAL_S}s, "
          f"window {RECOVERY_WINDOW_S}s, stall threshold {BATTERY_HARD_CAP_S}s\n")

    stalls = 0
    ports: dict[str, Port] = {}
    # Labgrid bench doctrine (AGENTS.md; Codex P1 on #107, issue #338):
    # acquire the DUT places BEFORE stopping pcscd or opening ports —
    # concurrent HIL/bolty/micronuts sessions cannot be raced by this
    # long-running probe. Partial acquisition releases what was held
    # and aborts (battery main() semantics).
    places = [LABGRID_PLACES[name] for name in ("nucula", "m5stick")]
    acquired = labgrid_acquire(places)
    if len(acquired) != len(places):
        labgrid_release(acquired)  # release exactly what was held
        print("could not acquire both DUT places — another session "
              "holds the bench; retry when free")
        return 2
    try:
        pcscd_stop()
        try:
            for name in ("nucula", "m5stick"):
                ports[name] = Port(name)
            for rname, port in ports.items():
                print(f"== {rname} ({port.s.port})")
                for cname, raw in cases:
                    latencies: list[float] = []
                    junks: list[int] = []
                    no_recovery = 0
                    for _ in range(args.iters):
                        try:
                            lat, junk = port.recovery_latency(raw)
                        except serial.SerialException:
                            port.reopen()  # nucula CDC readiness quirk — one clean cycle
                            try:
                                lat, junk = port.recovery_latency(raw)
                            except serial.SerialException:
                                lat, junk = None, 0
                        if lat is None:
                            no_recovery += 1
                        else:
                            latencies.append(lat)
                            junks.append(junk)
                    if latencies:
                        line = (f"  {cname:20s} n={len(latencies):3d} "
                                f"min={min(latencies):.2f}s med={statistics.median(latencies):.2f}s "
                                f"p95={pct(latencies, 0.95):.2f}s max={max(latencies):.2f}s "
                                f"junk-ahead-max={max(junks)}B")
                    else:
                        line = f"  {cname:20s} n=0 — NO RECOVERY in any iteration"
                    if no_recovery:
                        line += f" NO-RECOVERY={no_recovery}"
                    over_cap = sum(1 for l in latencies if l > BATTERY_HARD_CAP_S)
                    if over_cap:
                        line += f" OVER-CAP={over_cap}"
                    stalls += no_recovery + over_cap
                    print(line)
        finally:
            for port in ports.values():
                try:
                    port.s.close()
                except Exception:
                    pass
            pcscd_start()
    finally:
        labgrid_release(acquired)

    if stalls:
        print(f"\nVERDICT: REAL STALL — {stalls} recoveries exceeded the "
              f"{BATTERY_HARD_CAP_S}s battery cap (issue #89 step 2: instrument the UART loop)")
        return 1
    print(f"\nVERDICT: BATTERY FALSE POSITIVE — all recoveries within "
          f"{BATTERY_HARD_CAP_S}s; the flag was the idle-terminated read, not the "
          "firmware (issue #89: close as timing)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
