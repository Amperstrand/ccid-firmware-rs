#!/usr/bin/env python3
"""gempc_sim — GemPC Twin serial-reader simulator on a pty (#virtualization lane).

Speaks the SAME serial CCID framing the firmware does (reference-locked in
crates/ccid-transport-serial + tests/hardware/nfc/conformance_battery.py):
  [SYNC 0x03][CTRL 0x06/0x15][10-byte CCID header][data][LRC = XOR]
at 115200 8N2, answering as a reader with a scripted virtual card behind
slot 0. Purpose:

  1. ZERO-HARDWARE serial-CCID testing: point a lane pcscd reader.conf at
     the printed pty path and the full pcscd -> libccidtwin -> serial-CCID
     -> card stack runs on the host.
  2. Wire oracle for #540-class diagnosis: run the same APDU through the
     sim and the real m5stick, diff the wire bytes — known-good against
     device-under-test.

Card brain mirrors hce_relay.py's modes (--respond fixed hex, --script
CAPDU=RAPDU table, default 9000; --atr default = the #551 bench card).

Usage:
  python3 gempc_sim.py                       # prints pty path, serves forever
  python3 gempc_sim.py --atr 3B... --script card.txt --once 10
"""
import argparse
import json
import os
import pty
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "nfc"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from conformance_battery import (  # noqa: E402
    MSG_ESCAPE, MSG_GET_SLOT_STATUS, MSG_ICC_POWER_OFF,
    MSG_ICC_POWER_ON, MSG_XFR_BLOCK, frame, lrc,
)
from gempc_t1_sim import T1Adversary, T1Endpoint, looks_like_block  # noqa: E402

# CCID message types per CCID_SPEC §6.1 (docs/CCID_SPEC_AUDIT.md — matches
# crates/ccid-protocol/src/types.rs; NOT the battery's legacy mislabels).
PC_GET_PARAMETERS = 0x6C
PC_SET_PARAMETERS = 0x61
PC_RESET_PARAMETERS = 0x6D
RDR_DATABLOCK = 0x80
RDR_SLOTSTATUS = 0x81
RDR_PARAMETERS = 0x82
RDR_ESCAPE = 0x83

# bStatus composition (CCID §6.2.2): bmICCStatus[1:0] | bmCommandStatus[7:6]
ICC_ACTIVE = 0x00
ICC_INACTIVE = 0x01
ICC_ABSENT = 0x02
CMD_FAILED = 0x40
CMD_TIME_EXT = 0x80

# The #551 bench card (T=1) — the exact ATR the #540 diagnosis targets.
DEFAULT_ATR = bytes.fromhex("3B858131FE458073C8211084")

# T=1 default parameters — REFERENCE-LOCKED to the firmware's
# DEFAULT_T1_PARAMS (crates/ccid-protocol/src/types.rs:120): the bytes a
# bench-proven reader reports. Fi/Di 372/1, guard 0, BWI/CWI, IFSC 32.
T1_PARAMS = bytes([0x11, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00])
PROTO_T1 = 0x01  # rides in bSpecific for RDR_to_PC_Parameters (CCID §6.1.5)


def parse_script(lines, source: str = "<script>") -> dict:
    """CAPDU_HEX=RAPDU_HEX lines; loud on anything ambiguous (hce_relay shape)."""
    table: dict = {}
    for n, raw in enumerate(lines, 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if "=" not in line:
            raise SystemExit(f"{source}:{n}: not CAPDU_HEX=RAPDU_HEX: {line!r}")
        capdu, rapdu = (side.strip() for side in line.split("=", 1))
        try:
            table[bytes.fromhex(capdu)] = bytes.fromhex(rapdu)
        except ValueError as e:
            raise SystemExit(f"{source}:{n}: bad hex ({e}): {line!r}")
    return table


class GemPcSim:
    """Reader-side state machine: frame in, frame out, one slot."""

    def __init__(self, atr: bytes, table: dict, fixed: bytes | None,
                 adversary: str | None = None,
                 self_marked: bool = False):
        self.atr = atr
        self.table = table
        self.fixed = fixed
        # ICC lifecycle fidelity (found via the battery's card-relative
        # cases): a present card reads PRESENT+INACTIVE (bmICCStatus 1)
        # until IccPowerOn — never ACTIVE-at-boot; the battery's
        # power_on_expect depends on exactly this pre-activation state.
        self.card_active = True   # seated (presence)
        self.card_powered = False  # bmICCStatus: 1 until PowerOn, 0 after
        self.exchanges = 0
        self.naks = 0
        # pcscd's internal T=1 stack speaks BLOCKS through XfrBlock
        # (libccidtwin is TPDU-level): the card side carries the endpoint
        # twin (gempc_t1 — port of the firmware's t1.rs). Direct-serial
        # consumers keep getting raw-APDU dispatch.
        self.t1 = T1Endpoint()
        self.adversary = T1Adversary(adversary, self.t1) if adversary else None
        self.self_marked = self_marked

    def _status(self, cmd: int = 0) -> int:
        if not self.card_active:
            return ICC_ABSENT | cmd
        if not self.card_powered:
            return ICC_INACTIVE | cmd
        return ICC_ACTIVE | cmd

    def _card_answer(self, apdu: bytes) -> bytes:
        self.exchanges += 1
        if looks_like_block(apdu):  # T=1 dialect (pcscd path)
            kind, out = self.t1.feed(apdu)
            if kind == "imm":
                return out
            if self.adversary is not None:
                return self.adversary.answer(out, self._plain_answer(out))
            return self.t1.card_response(self._plain_answer(out))
        return self._plain_answer(apdu)

    def _plain_answer(self, apdu: bytes) -> bytes:
        if self.fixed is not None:
            return self.fixed
        if apdu in self.table:
            return self.table[apdu]
        if self.self_marked and len(apdu) >= 2:
            # Self-marked doctrine (#261): the vpicc_table's synthetic
            # GP-smoke answers — one source of truth for the
            # never-real-crypto shapes (marker + A5/5A filler), the
            # same answers on every emulated surface. Lazy import: the
            # sibling dir is not a package.
            sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "nfc"))
            from vpicc_table import default_table
            prefix_table = default_table()
            key = (apdu[0], apdu[1])
            if key in prefix_table:
                return prefix_table[key]
        return bytes.fromhex("6a82")

    def on_frame(self, msg_type: int, seq: int, payload: bytes) -> bytes | None:
        """Return the full wire response (frame()'d), or None for silence.
        bStatus/bError/bSpecific ride in the CCID HEADER (frame()'s
        extended args); data carries only ATR / APDU answer / params."""
        if msg_type == MSG_GET_SLOT_STATUS:
            return frame(RDR_SLOTSTATUS, seq, b"", bstatus=self._status())
        if msg_type == MSG_ICC_POWER_ON:
            if not self.card_active:
                return frame(RDR_SLOTSTATUS, seq, b"",
                             bstatus=self._status(CMD_FAILED))
            self.card_powered = True
            return frame(RDR_DATABLOCK, seq, self.atr,
                         bstatus=self._status())
        if msg_type == MSG_ICC_POWER_OFF:
            self.card_powered = False  # present+inactive again (§6.2.2)
            return frame(RDR_SLOTSTATUS, seq, b"", bstatus=self._status())
        if msg_type == MSG_XFR_BLOCK:
            if not self.card_active or not self.card_powered:
                return frame(RDR_SLOTSTATUS, seq, b"",
                             bstatus=self._status(CMD_FAILED))
            return frame(RDR_DATABLOCK, seq, self._card_answer(payload),
                         bstatus=self._status())
        if msg_type in (PC_GET_PARAMETERS, PC_SET_PARAMETERS,
                        PC_RESET_PARAMETERS):
            return frame(RDR_PARAMETERS, seq, T1_PARAMS,
                         bstatus=self._status(), bspecific=PROTO_T1)
        if msg_type == MSG_ESCAPE:
            ident = b"GemPC Twin SIM 1.0"
            if payload[:1] == bytes([0x02]):
                return frame(RDR_ESCAPE, seq, ident, bstatus=self._status())
            return frame(RDR_ESCAPE, seq, b"", bstatus=self._status())
        # Unknown type: cmd_not_supported, named per the honest-error contract
        return frame(RDR_SLOTSTATUS, seq, b"",
                     bstatus=self._status(CMD_FAILED), berror=0xFF)


def serve(master_fd: int, sim: GemPcSim, once: int = 0,
          capture=None) -> None:
    """Reader loop with GemPC-Twin ECHO semantics (reference:
    reference/CCID/src/ccid_serial.c — the Twin echoes received bytes on
    the line; libccidtwin's ReadSerial discards one echo-frame on its
    FIRST read (`echo = true`, consumed at the first successful frame).
    The bench m5stick echoes the same way (AGENTS: 'the m5stick echoes
    interleaved'). Without the echo, libccidtwin's open-handshake
    CmdEscape (firmware query) times out — found live against a real
    lane pcscd.)"""
    os.set_blocking(master_fd, False)
    buf = b""
    while True:
        try:
            chunk = os.read(master_fd, 4096)
            if chunk:
                buf += chunk
        except BlockingIOError:
            pass
        # manual frame scan (the raw consumed bytes ARE the echo payload)
        i = 0
        while i + 13 <= len(buf):
            if buf[i] != 0x03:  # SYNC
                i += 1
                continue
            dlen = int.from_bytes(buf[i + 3:i + 7], "little")
            end = i + 13 + dlen
            if end > len(buf):
                # Stray-SYNC restart (reference: ccid-transport-serial
                # FrameParser — "a stray SYNC means the previous SYNC
                # was garbage: restart collection on the new SYNC").
                # Without this, a truncated/oversized dwLength wedges
                # the scanner forever waiting for bytes that never
                # come — found live by the battery's fuzz leg.
                nxt = buf.find(b"\x03", i + 1)
                if nxt == -1 and len(buf) - i < 13 + 4096:
                    break  # plausible partial — wait for more bytes
                i += 1  # declared length is unbelievable: resync scan
                continue
            if lrc(buf[i:end - 1]) != buf[end - 1]:
                i += 1
                continue
            wire = buf[i:end]
            i = end
            if capture is not None:
                capture.write(json.dumps(
                    {"event": "rx", "t": time.time(),
                     "data": wire.hex()}) + "\n")
            os.write(master_fd, wire)  # the ECHO (line semantics)
            mt, seq, payload = wire[2], wire[8], wire[12:12 + dlen]
            resp = sim.on_frame(mt, seq, payload)
            if resp is not None:
                if capture is not None:
                    capture.write(json.dumps(
                        {"event": "tx", "t": time.time(),
                         "data": resp.hex()}) + "\n")
                os.write(master_fd, resp)
            if once and sim.exchanges >= once:
                # Drain grace: closing the pty master discards unread
                # slave-side data (the final response lost to the close
                # race — found live via the test's third exchange).
                time.sleep(0.3)
                return
        buf = buf[i:]
        if once is None:
            time.sleep(0.01)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--atr", default=DEFAULT_ATR.hex(),
                    help="card ATR hex (default: the #551 bench card)")
    ap.add_argument("--respond", metavar="HEX",
                    help="answer every C-APDU with this fixed R-APDU")
    ap.add_argument("--script", type=argparse.FileType("r"),
                    help="CAPDU_HEX=RAPDU_HEX table file (unmatched -> 6A82)")
    ap.add_argument("--once", type=int, default=0, metavar="N",
                    help="exit after N card exchanges (0 = forever)")
    ap.add_argument("--self-marked", action="store_true",
                    help="default card = the vpicc_table synthetic "
                         "GP-smoke answers (marker + A5/5A filler — "
                         "never real crypto; the #261 doctrine)")
    ap.add_argument("--capture", default=None, metavar="FILE",
                    help="JSONL wire capture: every frame in/out with "
                         "timestamps (the #540 wire-oracle harness)")
    ap.add_argument("--t1-adversary", default=None,
                    metavar="PROFILE",
                    choices=["wtx-storm", "seq-violation", "chain-abort"],
                    help="T=1 misbehavior instrument (host-stack "
                         "characterization; wtx-storm[:m], seq-violation)")
    args = ap.parse_args(argv)

    table = parse_script(args.script) if args.script else {}
    fixed = bytes.fromhex(args.respond) if args.respond else None
    sim = GemPcSim(bytes.fromhex(args.atr), table, fixed,
                   adversary=args.t1_adversary,
                   self_marked=args.self_marked)

    master, slave = pty.openpty()
    print(Path(os.ttyname(slave)), flush=True)  # the reader.conf DEVICENAME
    capture = None
    if args.capture:
        capture = open(args.capture, "a", buffering=1)
        capture.write(json.dumps({"event": "session", "atr": args.atr,
                                  "t": time.time()}) + "\n")
    serve(master, sim, args.once or None, capture=capture)
    return 0


if __name__ == "__main__":
    sys.exit(main())
