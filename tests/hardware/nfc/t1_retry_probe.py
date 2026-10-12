#!/usr/bin/env python3
"""T=1 retry-idempotence probe for the m5stick serial CCID reader (issue #101).

Proves, over the raw GemPC wire against the live card (zero card-state
risk: every APDU is a SELECT of a nonexistent AID — pure read):

  1. a retransmitted final I-block returns the CACHED answer
     byte-identically, with NO second card exchange (Escape 0xD0
     apdu_tx delta == 0) — the double-INSTALL hazard guard;
  2. a chained 33-byte APDU (I#1 M=1 32B -> R-ACK -> I#2 M=0 1B — the
     exact class that killed the 2026-10-11 CAP install) completes with
     the card answer delivered;
  3. a 5x retry storm on both chain pieces replays identically;
  4. a fresh N(S) afterwards still executes (apdu_tx +1).

Requires exclusive port access: stop pcscd first
  flock /run/lock/bench-pcscd.lock systemctl stop pcscd.socket pcscd.service
and restart it afterwards.

Usage:
  python3 t1_retry_probe.py            # full proof, exits nonzero on failure
  python3 t1_retry_probe.py --port /dev/ttyUSB0
"""
import argparse
import struct
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # tests/hardware

import serial  # noqa: E402

from nfc.conformance_battery import (  # noqa: E402
    SYNC, CTRL_ACK, lrc, parse_frames,
)
from serial_ports import resolve  # noqa: E402

T1_PARAMS = bytes([0x11, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00])


def open_port(port: str) -> serial.Serial:
    # safe opener: line state deasserted BEFORE open (bench rule — an
    # asserted-DTR open toggles the FTDI auto-reset line and reboots the
    # ESP32 mid-session)
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


def i_pcb(ns: int, more: bool, nr: int) -> int:
    return (ns << 6) | ((1 if more else 0) << 5) | (nr << 4)


def t1_block(pcb: int, inf: bytes) -> bytes:
    b = bytes([0x00, pcb, len(inf)]) + inf
    return b + bytes([lrc(b)])


def ccid(msg_type: int, seq: int, payload: bytes = b"", proto: int = 0) -> bytes:
    # CCID header: TYPE LEN(4LE) SLOT SEQ SPEC0(protocol) 0 0
    return (
        bytes([msg_type])
        + len(payload).to_bytes(4, "little")
        + bytes([0, seq, proto, 0, 0])
        + payload
    )


def wire_frame(ccid_msg: bytes) -> bytes:
    body = bytes([SYNC, CTRL_ACK]) + ccid_msg
    return body + bytes([lrc(body)])


class Wire:
    def __init__(self, port: str):
        self.port = port
        self.s = open_port(port)

    def reopen(self):
        try:
            self.s.close()
        except Exception:
            pass
        self.s = open_port(self.port)

    def rx(self, seconds: float) -> bytes:
        out = b""
        end = time.time() + seconds
        while time.time() < end:
            try:
                out += self.s.read(4096)
            except serial.SerialException:
                # third-party open of the shared tty EIOs our fd; reopen
                # without toggling lines (firmware state survives). Bytes
                # already in the dead fd's kernel buffer are lost — a
                # frame split by the reopen shows up as a headerless
                # tail, so exchange() retries once when nothing parses.
                print("  [wire EIO — reopening]")
                self.reopen()
        return out

    def exchange(self, ccid_msg: bytes, seconds: float, label: str):
        for attempt in range(2):
            self.s.reset_input_buffer()
            self.s.write(wire_frame(ccid_msg))
            d = self.rx(seconds)
            fs = parse_frames(d)
            if fs:
                return fs
            print(f"[{label}] attempt {attempt}: no frames (raw={d[:60].hex()})")
        print(f"[{label}] NO RESPONSE")
        return []


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", default=None)
    args = ap.parse_args()
    port = args.port or resolve("m5stick")

    w = Wire(port)
    time.sleep(3.0)  # boot/purge window after any reboot
    for attempt in range(10):
        if w.exchange(ccid(0x65, 80 + attempt), 1.5, "warmup"):
            break
        time.sleep(1.0)
    w.s.reset_input_buffer()

    def datablock_answers(fs):
        return [payload for (mtype, _, _, _, payload) in fs if mtype == 0x80]

    def escape_payloads(fs):
        return [payload for (mtype, _, _, _, payload) in fs if mtype == 0x83]

    # power cycle any stale activation, then power on (marginal coupling
    # makes single-shot activation a coin flip — bench #59/#89 class)
    w.exchange(ccid(0x63, 70), 3.0, "poweroff")
    time.sleep(0.5)
    atrs = []
    for attempt in range(4):
        fs = w.exchange(ccid(0x62, 1 + attempt), 20.0, f"poweron#{attempt}")
        atrs = datablock_answers(fs)
        if atrs:
            break
        w.exchange(ccid(0x63, 71 + attempt), 2.0, "retry-off")
        time.sleep(1.0)
    assert atrs, "no ATR across poweron attempts"
    print(f"ATR ({len(atrs[-1])}B): {atrs[-1].hex()}")

    fs = w.exchange(ccid(0x61, 2, T1_PARAMS, proto=1), 4.0, "setparams")
    assert any(mtype == 0x82 for (mtype, _, _, _, _) in fs), f"setparams: {fs}"
    print("SetParameters(T=1) OK")

    def diag(seq: int) -> dict:
        fs = w.exchange(ccid(0x6B, seq, bytes([0xD0])), 3.0, "0xD0")
        ps = escape_payloads(fs)
        assert ps, f"0xD0: {fs}"
        names = ["apdu_tx", "apdu_rx", "nak", "err", "reinit", "present", "uptime"]
        return dict(zip(names, struct.unpack("<7I", ps[-1][:28])))

    def t1_xfr(seq: int, t1_payload: bytes, label: str) -> bytes:
        """XfrBlock round-trip; grants S(WTX request) dances (G-04) and
        returns the final T=1 answer block."""
        fs = w.exchange(ccid(0x6F, seq, t1_payload), 15.0, label)
        answers = datablock_answers(fs)
        assert answers, f"{label}: {fs}"
        ans = answers[-1]
        for _ in range(4):
            if len(ans) >= 4 and ans[1] == 0xC3:
                grant = t1_block(0xE3, bytes([ans[3]]))  # S(WTX response m)
                fs = w.exchange(ccid(0x6F, (seq + 0x40) & 0xFF, grant),
                                15.0, f"{label}/wtx-grant")
                answers = datablock_answers(fs)
                assert answers, f"{label}/wtx: {fs}"
                ans = answers[-1]
            else:
                break
        return ans

    # 1. single I-block: SELECT of a nonexistent AID (pure read)
    sel = bytes([0x00, 0xA4, 0x04, 0x00, 0x06, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
    ib = t1_block(i_pcb(0, False, 0), sel)
    ans1 = t1_xfr(10, ib, "I(0) SELECT")
    inf1 = ans1[3 : 3 + ans1[2]]
    assert ans1[1] & 0x80 == 0 and inf1[-2:] == b"\x6a\x82", f"answer {ans1.hex()}"
    print(f"answer1: {ans1.hex()} (I-block, INF tail 6A82)")
    d0 = diag(11)

    # 2. retransmitted final I-block x3 -> cached replay, no card exchange
    for i in range(3):
        replay = t1_xfr(12 + i, ib, f"retransmit#{i}")
        assert replay == ans1, f"replay mismatch: {replay.hex()} != {ans1.hex()}"
    d1 = diag(15)
    print(f"3 retransmits byte-identical; apdu_tx {d0['apdu_tx']} -> {d1['apdu_tx']}")
    assert d1["apdu_tx"] == d0["apdu_tx"] and d1["apdu_rx"] == d0["apdu_rx"], (
        f"CACHE MISS — APDU re-executed: {d0} -> {d1}"
    )

    # 3. chained 33-byte APDU (the 2026-10-11 CAP-install killer class)
    aid33 = bytes([0x00, 0xA4, 0x04, 0x00, 0x1C]) + bytes(range(0x10, 0x2C))
    assert len(aid33) == 33
    p1, p2 = aid33[:32], aid33[32:]
    ack = t1_xfr(20, t1_block(i_pcb(1, True, 0), p1), "chain I#1")
    assert ack[1] & 0xC0 == 0x80 and (ack[1] >> 4) & 1 == 1, f"R(1) ACK: {ack.hex()}"
    ans2 = t1_xfr(21, t1_block(i_pcb(1, False, 0), p2), "chain I#2")
    inf2 = ans2[3 : 3 + ans2[2]]
    assert ans2[1] & 0x80 == 0 and inf2[-2:] == b"\x6a\x82", f"chain {ans2.hex()}"
    print("chained 33B APDU delivered (R-ACK + card answer)")

    # 4. retry storm x5 on both chain pieces
    for i in range(5):
        r_ack = t1_xfr(30 + 2 * i, t1_block(i_pcb(1, True, 0), p1), f"storm I#1 #{i}")
        assert r_ack == ack, f"re-ACK mismatch: {r_ack.hex()} != {ack.hex()}"
        r_ans = t1_xfr(31 + 2 * i, t1_block(i_pcb(1, False, 0), p2), f"storm I#2 #{i}")
        assert r_ans == ans2, f"final replay mismatch: {r_ans.hex()} != {ans2.hex()}"
    d2 = diag(50)
    print(f"5x retry storm identical; apdu_tx {d1['apdu_tx']} -> {d2['apdu_tx']}")
    assert d2["apdu_tx"] == d1["apdu_tx"] + 1, f"chained APDU count: {d1} -> {d2}"
    assert d2["nak"] == d0["nak"] and d2["err"] == d0["err"], f"wire errors: {d0} -> {d2}"

    # 5. fresh N(S) still executes
    fresh_sel = bytes([0x00, 0xA4, 0x04, 0x00, 0x02, 0x31, 0x32])
    ans3 = t1_xfr(60, t1_block(i_pcb(0, False, 1), fresh_sel), "fresh I(0)")
    inf3 = ans3[3 : 3 + ans3[2]]
    assert ans3[1] & 0x80 == 0 and inf3[-2:] == b"\x6a\x82", f"fresh {ans3.hex()}"
    d3 = diag(61)
    assert d3["apdu_tx"] == d2["apdu_tx"] + 1, f"fresh exchange: {d2} -> {d3}"

    print("\nT=1 RETRY PROBE: ALL PASS")
    w.s.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
