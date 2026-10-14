#!/usr/bin/env python3
"""WTX bench gate for the nucula PN7160 serial CCID reader (issue #156 E).

Hammer ×N zero-risk card exchanges (ISD-lock doctrine, AGENTS.md
"Card-Handling Doctrine" — SELECT by AID / GET DATA CPLC ONLY; no
SC-class command is ever sent):

  - worst-case XfrBlock answer time <= 2.5 s (acceptance bound),
  - every response parses with the right bSeq (no post-exchange
    desync — the #101/#151 failure class),
  - interim CCID time-extension frames (issue #156 B) are counted and
    shape-checked (type 0x80 DataBlock mirroring the in-flight
    XfrBlock's response type per CCID 1.1 §6.1.4 — pre-fix 0x81
    SlotStatus interims also tolerated; bStatus 0x80|icc, bError > 0),
  - Escape 0xD4 (issue #156 A3) WTX counters read before/after.

Direct-serial GemPC framing over the USB-CDC with the safe opener
(the nucula's documented test surface). Requires exclusive port
access: stop the nucula-ccid-broker service first.

Usage:
  python3 wtx_hammer.py [--port /dev/ttyACM0] [--rounds 100]

Exit codes: 0 pass, 1 gate failure, 2 card-gated (no card on coil).
"""
import argparse
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # tests/hardware

import serial  # noqa: E402

from nfc.conformance_battery import CTRL_ACK, SYNC, lrc, parse_frames  # noqa: E402

# Zero-risk APDU set only (AGENTS.md safe/unsafe table):
PPSE_SELECT = bytes.fromhex("00A404000E325061792E5359532E4444463031")  # 2PAY.SYS.DDF01
GET_DATA_CPLC = bytes.fromhex("80CA9F7F00")
T1_PARAMS = bytes([0x11, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00])

PC_TO_RDR_GET_SLOT_STATUS = 0x65
PC_TO_RDR_ICC_POWER_ON = 0x62
PC_TO_RDR_XFR_BLOCK = 0x6F
PC_TO_RDR_SET_PARAMETERS = 0x61
PC_TO_RDR_ESCAPE = 0x6B
RDR_TO_PC_DATABLOCK = 0x80
RDR_TO_PC_SLOTSTATUS = 0x81
RDR_TO_PC_NOTIFY_SLOT_CHANGE = 0x50
TIME_EXTENSION_BIT = 0x80
# Interim time-extension carriers (Codex PR159 P2 + fix 2): current
# firmware emits 0x80 (the in-flight XfrBlock's response type, CCID
# 1.1 §6.1.4); 0x81 interims from pre-fix firmware are tolerated.
INTERIM_TYPES = (RDR_TO_PC_DATABLOCK, RDR_TO_PC_SLOTSTATUS)


def open_port(port: str) -> serial.Serial:
    # Safe opener: DTR/RTS deasserted BEFORE open (C3 USB-Serial/JTAG
    # chip-resets on host line assertions — AGENTS.md nucula rules).
    s = serial.Serial()
    s.port = port
    s.baudrate = 115200
    s.bytesize = 8
    s.parity = serial.PARITY_NONE
    s.stopbits = 2
    s.dtr = False
    s.rts = False
    s.timeout = 0.1
    s.open()
    return s


def ccid(msg_type: int, seq: int, payload: bytes = b"", proto: int = 0) -> bytes:
    return (
        bytes([msg_type])
        + len(payload).to_bytes(4, "little")
        + bytes([0, seq, proto, 0, 0])
        + payload
    )


def wire_frame(ccid_msg: bytes) -> bytes:
    body = bytes([SYNC, CTRL_ACK]) + ccid_msg
    return body + bytes([lrc(body)])


def i_block(ns: int, inf: bytes) -> bytes:
    pcb = (ns << 6)  # I(N(S)=ns, M=0)
    b = bytes([0x00, pcb, len(inf)]) + inf
    return b + bytes([lrc(b)])


def is_final_response(fr) -> bool:
    """A final reader response: an RDR-to-PC message type (>= 0x80,
    not the 0x50 slot-change notify) whose bStatus carries NO
    time-extension bit. Interim time-extension frames (issue #156 B)
    carry the SAME bSeq as the in-flight command — the extension bit,
    not the sequence, is what distinguishes them."""
    mt, _seq, bstatus, _berror, _pl = fr
    return (
        mt >= 0x80
        and mt != RDR_TO_PC_NOTIFY_SLOT_CHANGE
        and not (bstatus & TIME_EXTENSION_BIT)
    )


def final_index(frames, seq):
    """Index of the final response FOR `seq`, or None. A qualifying
    frame with a foreign bSeq (stale residue) does not terminate the
    wait (Codex PR159 P2): the response for THIS command may still be
    in flight."""
    for idx in range(len(frames) - 1, -1, -1):
        if is_final_response(frames[idx]) and frames[idx][1] == seq:
            return idx
    return None


class Gate:
    def __init__(self, port: str):
        self.port = port
        self.s = open_port(port)
        self.seq = 0
        self.interim_frames = 0
        self.bad_interims = 0
        self.desyncs = 0
        self.no_response = 0
        self.times = []  # (label, seq, seconds)

    def exchange(self, msg_type: int, payload: bytes, label: str,
                 proto: int = 0, budget: float = 6.0):
        """Send one framed CCID command; return the parsed frames
        (echo first, interims between, final response LAST). Gate-
        failing conditions are counted, never fatal mid-run."""
        self.seq = (self.seq + 1) & 0xFF
        seq = self.seq
        self.s.reset_input_buffer()
        t0 = time.monotonic()
        self.s.write(wire_frame(ccid(msg_type, seq, payload, proto)))
        frames = []
        deadline = time.monotonic() + budget
        while time.monotonic() < deadline:
            frames += parse_frames(self.s.read(4096))
            # Codex PR159 P2: terminate ONLY on a non-time-extension
            # reader response for THIS sequence — during a stall the
            # first two frames are echo + interim time-extension, and
            # treating the interim as final discarded the real response
            # via the next command's reset_input_buffer().
            if final_index(frames, seq) is not None:
                break
        took = time.monotonic() - t0
        self.times.append((label, seq, took))
        end = final_index(frames, seq)
        if end is None:
            # An echo-only (or interim-only) timeout is NOT a response
            # (Codex PR159 P2) — never treat the echo or an interim
            # extension as the final answer.
            detail = f"{len(frames)} frame(s) before timeout"
            print(f"  [{label}] seq={seq}: NO FINAL RESPONSE ({detail})")
            self.no_response += 1
            if any(is_final_response(f) for f in frames):
                self.desyncs += 1
                print(f"  [{label}] DESYNC: only foreign-bSeq finals seen")
            return []
        # frames[0] is the echo (same type as the command); anything
        # between echo and final must be a well-formed interim
        # time-extension frame (issue #156 B).
        for mt, _fseq, bstatus, berror, _pl in frames[1:end]:
            self.interim_frames += 1
            if mt not in INTERIM_TYPES or not (bstatus & TIME_EXTENSION_BIT) or berror == 0:
                self.bad_interims += 1
                print(f"  [{label}] BAD INTERIM: mt={mt:#x} bstatus={bstatus:#x} berror={berror:#x}")
        return frames


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", default="/dev/ttyACM0")
    ap.add_argument("--rounds", type=int, default=100)
    args = ap.parse_args()

    print(f"== issue #156 E bench gate — {args.port}, {args.rounds} rounds ==")
    g = Gate(args.port)

    # Let the boot banner drain, then poll card presence.
    time.sleep(4.0)
    g.s.reset_input_buffer()
    present = False
    for _ in range(10):
        fs = g.exchange(PC_TO_RDR_GET_SLOT_STATUS, b"", "gss")
        if fs and (fs[-1][2] & 0x03) != 0x02:  # ICC absent == 0x02
            present = True
            break
        time.sleep(0.5)
    if not present:
        print("CARD-GATED: no ICC present (card not on coil / not coupling)")
        return 2

    fs = g.exchange(PC_TO_RDR_ICC_POWER_ON, b"", "power-on")
    print(f"ATR: {fs[-1][4].hex().upper() if fs else '<none>'}")

    g.exchange(PC_TO_RDR_SET_PARAMETERS, T1_PARAMS, "set-params", proto=1)

    fs = g.exchange(PC_TO_RDR_ESCAPE, b"\xD4", "wtx-before")
    print(f"WTX before: {fs[-1][4].hex() if fs else '<none>'}")

    ns = 0
    for i in range(args.rounds):
        apdu = PPSE_SELECT if i % 2 == 0 else GET_DATA_CPLC
        g.exchange(PC_TO_RDR_XFR_BLOCK, i_block(ns, apdu), f"xfr[{i}]")
        ns ^= 1

    fs = g.exchange(PC_TO_RDR_ESCAPE, b"\xD4", "wtx-after")
    print(f"WTX after:  {fs[-1][4].hex() if fs else '<none>'}")

    xfr_times = [t for t in g.times if t[0].startswith("xfr")]
    worst = max(t[2] for t in xfr_times) if xfr_times else 0.0
    over = sum(1 for t in xfr_times if t[2] > 2.5)
    print(f"XfrBlock exchanges={len(xfr_times)} worst={worst:.3f}s over-budget={over}")
    print(f"interim time-extension frames: {g.interim_frames} (bad: {g.bad_interims})")
    print(f"desyncs: {g.desyncs}  no-response: {g.no_response}")

    verdict = (
        worst <= 2.5
        and over == 0
        and g.desyncs == 0
        and g.no_response == 0
        and g.bad_interims == 0
        and len(xfr_times) == args.rounds
    )
    print("GATE:", "PASS" if verdict else "FAIL")
    return 0 if verdict else 1


if __name__ == "__main__":
    sys.exit(main())
