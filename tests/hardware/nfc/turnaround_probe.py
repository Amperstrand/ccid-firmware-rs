#!/usr/bin/env python3
"""Turnaround split probe for the m5stick serial CCID reader (issue #120).

Splits the per-command latency into:
  host RTT (write -> full response on the wire, measured here, no pcscd)
  firmware turnaround (last cmd byte received -> response flushed, Escape 0xD2)
  card exchange (inside transmit_apdu, Escape 0xD2)

Measurement rules (issue #119 cdc-burst lesson): responses are read in
inWaiting()-sized chunks only — never a fixed-size blocking read; RTT is
stamped the moment the complete response frame is parsed.

Usage:
  python3 turnaround_probe.py            # GetSlotStatus + (if card) SELECT
  python3 turnaround_probe.py --n 50
  python3 turnaround_probe.py --status-only
"""
import argparse
import statistics
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # tests/hardware

import serial  # noqa: E402

from nfc.conformance_battery import (  # noqa: E402
    CTRL_ACK, MSG_ESCAPE, MSG_GET_SLOT_STATUS, MSG_ICC_POWER_ON, MSG_XFR_BLOCK,
    SYNC, frame, lrc, parse_frames,
)
from serial_ports import resolve  # noqa: E402

MSG_ESCAPE_D2 = bytes([0xD2])
SELECT_APDU = bytes([0x00, 0xA4, 0x04, 0x00, 0x00])


def open_port(port: str) -> serial.Serial:
    s = serial.Serial(port, 115200, bytesize=8, parity="N", stopbits=2, timeout=0.5)
    s.setDTR(False)
    s.setRTS(False)
    return s


def drain(s: serial.Serial, budget_s: float = 0.6) -> bytes:
    out = b""
    t0 = time.time()
    while time.time() - t0 < budget_s:
        n = s.in_waiting
        if n == 0:
            break
        out += s.read(n)
    return out


def exchange_timed(s: serial.Serial, seq: int, msg_type: int, data: bytes,
                   timeout_s: float = 2.0):
    """Send one framed command; return (rtt_ms, [frames], raw).

    RTT is stamped when a complete LRC-valid RDR frame carrying our seq
    has arrived (chunks are inWaiting-sized; the read loop exits as soon
    as the response is complete).
    """
    payload = frame(msg_type, seq, data)
    drain(s)
    t0 = time.perf_counter()
    s.write(payload)
    buf = b""
    deadline = t0 + timeout_s
    rtt_ms = None
    while time.perf_counter() < deadline:
        n = s.in_waiting
        if n:
            buf += s.read(n)
            frames = parse_frames(buf)
            if any(f[0] >= 0x80 and f[1] == seq for f in frames):
                rtt_ms = (time.perf_counter() - t0) * 1000.0
                break
        else:
            time.sleep(0.001)
    frames = parse_frames(buf)
    resp = [f for f in frames if f[0] >= 0x80 and f[1] == seq]
    return rtt_ms, resp, buf


def read_0xd2(s: serial.Serial, seq: int):
    rtt, resp, _ = exchange_timed(s, seq, MSG_ESCAPE, MSG_ESCAPE_D2)
    for f in resp:
        if f[0] == 0x83 and len(f[4]) >= 16:  # RDR_to_PC_Escape
            words = [int.from_bytes(f[4][i:i + 4], "little")
                     for i in range(0, min(len(f[4]), 32), 4)]
            return words, rtt
    return None, rtt


def stats_line(tag, words):
    if words is None:
        return f"{tag}: NO 0xD2 RESPONSE"
    last_ta, max_ta, last_card, max_card = words[:4]
    line = (f"{tag}: turnaround last={last_ta/1000:8.3f} max={max_ta/1000:8.3f} ms | "
            f"card last={last_card/1000:8.3f} max={max_card/1000:8.3f} ms")
    if len(words) >= 8:
        blocks, wtx, rec, max_xcvr = words[4:8]
        line += (f" | xcvr blocks={blocks} wtx={wtx} rec={rec}"
                 f" max_block={max_xcvr/1000:7.3f} ms")
    return line


def run_pass(s: serial.Serial, n: int, do_card: bool):
    seq = 0

    def next_seq():
        nonlocal seq
        seq = seq % 255 + 1
        return seq

    print(stats_line("before any command      ", read_0xd2(s, next_seq())[0]))

    rtts = []
    for _ in range(n):
        rtt, resp, raw = exchange_timed(s, next_seq(), MSG_GET_SLOT_STATUS, b"")
        if rtt is None:
            print(f"  GetSlotStatus NO RESPONSE (raw tail: {raw[-32:].hex()})")
            break
        rtts.append(rtt)
    if rtts:
        rs = sorted(rtts)
        print(f"GetSlotStatus raw host RTT: n={len(rs)} p50={rs[len(rs)//2]:7.3f} "
              f"min={rs[0]:7.3f} max={rs[-1]:7.3f} ms")
    print(stats_line("after GetSlotStatus x%d  " % len(rtts), read_0xd2(s, next_seq())[0]))

    if not do_card:
        return

    rtt, resp, _ = exchange_timed(s, next_seq(), MSG_ICC_POWER_ON, b"")
    if not resp or resp[0][2] & 0xC0:
        print(f"IccPowerOn: no card (bStatus={resp[0][2]:#04x}) — skipping SELECT pass")
        return
    atr = resp[0][4]
    print(f"IccPowerOn: ATR ({len(atr)} B) {atr.hex()}")

    rtts = []
    for _ in range(n):
        rtt, resp, _ = exchange_timed(s, next_seq(), MSG_XFR_BLOCK, SELECT_APDU)
        if rtt is None:
            print("  SELECT NO RESPONSE")
            break
        rtts.append(rtt)
    if rtts:
        rs = sorted(rtts)
        print(f"SELECT raw host RTT:       n={len(rs)} p50={rs[len(rs)//2]:7.3f} "
              f"min={rs[0]:7.3f} max={rs[-1]:7.3f} ms  sw={resp[0][4][-2:].hex() if resp else '?'}")
    print(stats_line("after SELECT x%d         " % len(rtts), read_0xd2(s, next_seq())[0]))


def latency_timer(port: str) -> str:
    try:
        base = Path(port).resolve()
        for sysfs in Path("/sys/bus/usb-serial/devices").iterdir():
            if Path(f"/dev/{sysfs.name}") == base:
                lt = sysfs / "latency_timer"
                if lt.exists():
                    return f"{sysfs.name} latency_timer={lt.read_text().strip()}"
        return "latency_timer: n/a"
    except Exception:
        return "latency_timer: ?"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=20)
    ap.add_argument("--status-only", action="store_true")
    ap.add_argument("--port", default=None)
    args = ap.parse_args()

    port = args.port or resolve("m5stick")
    print(f"port: {port} ({latency_timer(port)})")
    s = open_port(port)
    try:
        time.sleep(0.5)
        stale = drain(s)
        if stale:
            print(f"drained {len(stale)} stale bytes: {stale[:48].hex()}")
        run_pass(s, args.n, not args.status_only)
    finally:
        drain(s)
        s.close()


if __name__ == "__main__":
    main()
