#!/usr/bin/env python3
"""gempc_t1 — raw GemPC-Twin wire harness with a full T=1 session driver.

Promoted from the 2026-10-20 bench-session scratch (t1_session.py /
t1_verify.py / replay_libccid.py): the exact libccidtwin session shape,
as maintained tooling with LRC/EDC verification on BOTH layers (GemPC
frame + T=1 block). See GEMPC_WIRE_HARNESS.md for when to use this
instead of the pcscd path.

Two surfaces, mirroring labgrid/gempc_smoke.py:

1. Library — pure framing + T=1 block primitives (stdlib only at
   module scope: the CI python-collect gate imports this file in a
   minimal env; pyserial is imported lazily inside the hardware
   functions) plus a T1Session driver for the wire.
2. CLI (`python3 gempc_t1.py --probe`) — GetSlotStatus -> IccPowerOn
   -> SetParameters(T=1) -> S(IFS request) -> SELECT, printing every
   parsed frame and T=1 block.

WIRE FACTS (bench-proven 2026-10-20, GemPC Twin over the m5stick FTDI):

* Frame = SYNC 0x03, CTRL 0x06, 10-byte CCID header (type, dwLength
  LE, slot, seq, bBWI/bProtocolNum, 2x RFU), payload (starts at byte
  12), XOR-LRC over all preceding bytes.
* Serial is 115200 8N2 — 8N1 is framing death, not even echo.
  USB-CDC readers (nucula c3) use 8N1: pass --stopbits 1.
* DTR/RTS must be LOW before open (gempc_smoke.open_safe) — an
  asserted-DTR open toggles the FTDI auto-reset line.
* libccidtwin session setup: GetSlotStatus -> IccPowerOn ->
  SetParameters with bProtocolNum=1 in the CCID header-tail byte
  index 3 (wire byte 9). A bare-payload frame leaves it 0 = T=0 and
  the T=1 endpoint NEVER ARMS — XFRs then go to the card as raw
  TPDUs, answered 6700/6E00. make_frame(..., proto=1) encodes this;
  test_gempc_t1 locks the byte position.
* The IFS request must carry its EDC: [00 C1 01 FE]+LRC. WITHOUT the
  EDC the endpoint correctly answers R(retransmit) and send_ifsc
  stays 32.
* The firmware is bilingual: bare-APDU XfrBlock payloads get bare
  DataBlock responses; T=1-block payloads get T=1-wrapped responses.
* T=1 answers may defer behind S(WTX request) [00 C3 01 m]+LRC —
  answer S(WTX response) [00 E3 01 m]+LRC in a further XfrBlock and
  the I-block follows (T1Session.xfr_block runs the dance).
* pcscd holds the port: all raw-wire work stops pcscd under the
  bench flock (esp32_flash.pcscd_lock_acquire/_release — the same
  dance the flash path uses).

T=1 PCB layout — mirrors firmware/esp32-ccid/src/t1.rs parse_pcb
(ISO 7816-3 §11.4.2 Table 45 as implemented by pcsc-lite proto-t1.c,
the engine libccidtwin actually runs):
  I-block: 0x00 | ns<<6 | more<<5 | nr<<4      (b8 = 0)
  R-block: 0x80 | nr<<4 | err                  (err 0 = R(ACK))
  S-block: 0xC0 | kind   (0=resync 1=IFS 2=abort 3=WTX),
           response sets 0x20 (WTX req C3 -> resp E3, IFS C1 -> E1)

Card-safety doctrine (AGENTS.md): this harness sends SELECT-class
APDUs only — never SC-class commands (INIT_UPDATE, EXT_AUTH, ...).

Usage:
  python3 gempc_t1.py --probe                          # m5stick, by-id
  python3 gempc_t1.py --probe --port /dev/ttyUSB0 --stopbits 1
  python3 gempc_t1.py --probe --aid D276000124010304AFAF000000000000
  python3 gempc_t1.py --probe --apdu 00CA006E          # extra APDUs
  python3 gempc_t1.py --probe --no-pcscd-lock          # pcscd already down
"""

from __future__ import annotations

import argparse
import fcntl
import os
import sys
import time
from pathlib import Path
from typing import NamedTuple

# --- GemPC wire constants (conformance_battery shape; cross-locked by
# test_gempc_t1 against conformance_battery.frame when pyserial exists) ---
SYNC = 0x03
CTRL_ACK = 0x06

MSG_SET_PARAMETERS = 0x61
MSG_ICC_POWER_ON = 0x62
MSG_ICC_POWER_OFF = 0x63
MSG_GET_SLOT_STATUS = 0x65
MSG_ESCAPE = 0x6B
MSG_XFR_BLOCK = 0x6F

RDR_DATA_BLOCK = 0x80
RDR_SLOT_STATUS = 0x81
RDR_PARAMETERS = 0x82
RDR_ESCAPE = 0x83

CCID_TYPE_NAMES = {
    0x61: "SetParameters",
    0x62: "IccPowerOn",
    0x63: "IccPowerOff",
    0x65: "GetSlotStatus",
    0x6B: "Escape",
    0x6C: "GetParameters",
    0x6D: "ResetParameters",
    0x6F: "XfrBlock",
    0x80: "DataBlock",
    0x81: "SlotStatus",
    0x82: "Parameters",
    0x83: "EscapeReply",
    0x84: "DataRateAndClock",
}

# libccidtwin's SetParameters(T=1) payload, captured from a pcscd
# strace 2026-10-20 and bench-proven (t1_session.py). t1_retry_probe
# used the variant 11 00 00 03 00 20 00 — both accepted by the
# endpoint; the arming byte is the header proto, not the payload.
SET_PARAMS_T1_PAYLOAD = bytes.fromhex("1110004500fe00")

# GREAT EXPECTATION (bench 2026-10-20): libccidtwin opens T=1 with
# S(IFS request 254) = 00 C1 01 FE + LRC. test_gempc_t1 locks the bytes.
DEFAULT_IFSC = 0xFE
S_IFS_REQUEST = 0xC1
S_IFS_RESPONSE = 0xE1
S_WTX_REQUEST = 0xC3
S_WTX_RESPONSE = 0xE3

# t1.rs MAX_IFSC: §11.6.2 IFS ceiling is 254.
MAX_IFSC = 254

# Default probe APDU: SELECT of the bench SmartPGP applet (pure read —
# card-safety doctrine allows unlimited SELECTs). Bench-proven 2026-10-20.
DEFAULT_SELECT_AID = bytes.fromhex("D276000124010304AFAF000000000000")

# SC-class APDUs the harness REFUSES (AGENTS.md card-handling doctrine):
# INITIALIZE UPDATE (80 50), EXTERNAL AUTHENTICATE (84 82), SC-gated
# GET DATA. The GET DATA tag rides P1/P2 (CLA/INS = 80 CA or 84 CA,
# P1P2 = 00 E0) — a prefix check on CLA/INS alone misses it (Codex P1
# on #347); the legacy 00 E0-CLA form is kept for belt and braces.
# Each is a budget-bearing authentication event on JCOP-class cards —
# never in a loop, never from tooling.
SC_CLASS_PREFIXES = (b"\x80\x50", b"\x84\x82", b"\x00\xe0")
GP_GET_DATA_INS = (0xCA,)
GP_GET_DATA_TAGS = (b"\x00\xe0",)


def is_sc_class_apdu(apdu: bytes) -> bool:
    if apdu[:2] in SC_CLASS_PREFIXES:
        return True
    if len(apdu) >= 4 and apdu[1] in GP_GET_DATA_INS and apdu[2:4] in GP_GET_DATA_TAGS:
        return True
    return False


def lrc(data: bytes) -> int:
    """XOR-LRC over all bytes (GemPC frame checksum AND T=1 EDC)."""
    x = 0
    for b in data:
        x ^= b
    return x


class Frame(NamedTuple):
    """One parsed GemPC wire frame (conformance_battery tuple shape)."""

    msg_type: int
    seq: int
    bstatus: int
    berror: int
    payload: bytes


def make_frame(msg_type: int, seq: int = 0, payload: bytes = b"",
               proto: int = 0) -> bytes:
    """SYNC|CTRL|CCID header|payload|LRC.

    `proto` rides the CCID header-tail byte index 3 (bBWI/bProtocolNum,
    wire byte 9). For SetParameters(T=1) proto=1 is what ARMS the T=1
    endpoint — the 2026-10-20 session root cause was exactly this byte
    left at 0 by a bare-payload frame builder.
    """
    header = bytes([msg_type])
    header += len(payload).to_bytes(4, "little")
    header += bytes([0, seq & 0xFF, proto & 0xFF, 0, 0])
    body = bytes([SYNC, CTRL_ACK]) + header + payload
    return body + bytes([lrc(body)])


def parse_frames(buf: bytes) -> list[Frame]:
    """SYNC-anchored, LRC-verified structural parse.

    Resyncs past non-frame bytes (shared CDC ports carry log noise —
    AGENTS.md "reader test timing" lesson): a wrong-LRC candidate is
    skipped byte-wise, never trusted.
    """
    out: list[Frame] = []
    i = 0
    while i < len(buf) - 12:
        if buf[i] != SYNC:
            i += 1
            continue
        dlen = int.from_bytes(buf[i + 3:i + 7], "little")
        total = 13 + dlen
        if total > len(buf) - i:
            i += 1
            continue
        if lrc(buf[i:i + total - 1]) != buf[i + total - 1]:
            i += 1
            continue
        out.append(Frame(buf[i + 2], buf[i + 8], buf[i + 9], buf[i + 10],
                         bytes(buf[i + 12:i + 12 + dlen])))
        i += total
    return out


def split_echo(buf: bytes, msg_type: int) -> tuple[Frame | None, list[Frame]]:
    """Split a raw capture into (echo frame, [RDR responses]).

    The device ECHOES the sent frame before replying; the echo of a
    host->reader command carries the command's own type (< 0x80),
    responses carry >= 0x80.
    """
    echo: Frame | None = None
    responses: list[Frame] = []
    for f in parse_frames(buf):
        if echo is None and f.msg_type == msg_type and f.msg_type < 0x80:
            echo = f
        else:
            responses.append(f)
    return echo, responses


# --------------------------------------------------------------------------
# T=1 blocks (ISO 7816-3 §11; layout mirrors firmware t1.rs — see module
# docstring for the PCB citation).
# --------------------------------------------------------------------------

class T1Block(NamedTuple):
    """One parsed T=1 block: NAD|PCB|LEN|INF|EDC with EDC verification."""

    nad: int
    pcb: int
    length: int          # raw LEN byte as received
    inf: bytes
    edc: int | None      # EDC byte when the block is complete
    complete: bool       # block long enough for LEN + EDC

    @property
    def edc_ok(self) -> bool:
        if self.edc is None:
            return False
        return lrc(bytes([self.nad, self.pcb, self.length]) + self.inf) \
            == self.edc

    @property
    def is_i(self) -> bool:
        return (self.pcb & 0x80) == 0

    @property
    def is_r(self) -> bool:
        return (self.pcb & 0xC0) == 0x80

    @property
    def is_s(self) -> bool:
        return (self.pcb & 0xC0) == 0xC0

    @property
    def ns(self) -> int:
        return (self.pcb >> 6) & 1

    @property
    def more(self) -> bool:
        return bool(self.pcb & 0x20)

    @property
    def nr(self) -> int:
        return (self.pcb >> 4) & 1

    @property
    def r_err(self) -> int:
        return self.pcb & 0x03

    @property
    def s_kind(self) -> str:
        return ("resync", "ifs", "abort", "wtx")[self.pcb & 0x03]

    @property
    def s_response(self) -> bool:
        return bool(self.pcb & 0x20)

    def is_wtx_request(self) -> bool:
        return self.is_s and not self.s_response and (self.pcb & 0x03) == 3

    def describe(self) -> str:
        if self.is_i:
            kind = f"I ns={self.ns} more={int(self.more)} nr={self.nr}"
        elif self.is_r:
            kind = f"R nr={self.nr} err={self.r_err}" \
                   + ("(ACK)" if self.r_err == 0 else "")
        else:
            kind = f"S({self.s_kind} " \
                   f"{'response' if self.s_response else 'request'})"
        if self.edc is None:
            verdict = "EDC=??" if not self.complete else "EDC=?"
        else:
            verdict = f"EDC={self.edc:02X} " \
                      f"{'OK' if self.edc_ok else '*** BAD ***'}"
        return (f"NAD={self.nad:02X} PCB={self.pcb:02X} [{kind}] "
                f"LEN={self.length:02X} INF[{len(self.inf)}] {verdict}")


def parse_t1(block: bytes) -> T1Block | None:
    """Structural + EDC parse of one T=1 block; None when < 4 bytes."""
    if len(block) < 4:
        return None
    nad, pcb, length = block[0], block[1], block[2]
    complete = len(block) >= 4 + length
    inf = bytes(block[3:3 + length])
    edc = block[3 + length] if complete else None
    return T1Block(nad, pcb, length, inf, edc, complete)


def t1_block(pcb: int, inf: bytes = b"", nad: int = 0x00) -> bytes:
    """NAD|PCB|LEN|INF|EDC — the EDC is load-bearing (see module docs)."""
    if len(inf) > MAX_IFSC:
        raise ValueError(f"INF {len(inf)}B exceeds the §11.6.2 IFS "
                         f"ceiling {MAX_IFSC} — chain I-blocks instead")
    b = bytes([nad, pcb, len(inf)]) + inf
    return b + bytes([lrc(b)])


def i_pcb(ns: int, more: bool, nr: int) -> int:
    return (ns << 6) | ((1 if more else 0) << 5) | (nr << 4)


def i_block(ns: int, nr: int, inf: bytes, more: bool = False) -> bytes:
    return t1_block(i_pcb(ns, more, nr), inf)


def r_block(nr: int, err: int = 0) -> bytes:
    return t1_block(0x80 | (nr << 4) | (err & 0x03))


def ifs_request(ifsc: int = DEFAULT_IFSC) -> bytes:
    """S(IFS request) — libccidtwin's session opener (bench-proven)."""
    return t1_block(S_IFS_REQUEST, bytes([ifsc]))


def wtx_response(m: int) -> bytes:
    """S(WTX response m) — grants the reader's S(WTX request m)."""
    return t1_block(S_WTX_RESPONSE, bytes([m]))


# --------------------------------------------------------------------------
# Session driver (hardware paths — pyserial imported lazily).
# --------------------------------------------------------------------------

class T1Error(RuntimeError):
    """Wire-level failure; str() carries the collected evidence."""


def _import_labgrid_module(name: str):
    """Import a tests/hardware/labgrid module without leaving its dir
    on sys.path (bare-name collisions across suite dirs, AGENTS.md)."""
    labdir = str(Path(__file__).resolve().parents[1] / "labgrid")
    sys.path.insert(0, labdir)
    try:
        import importlib
        return importlib.import_module(name)
    finally:
        try:
            sys.path.remove(labdir)
        except ValueError:
            pass


def _default_port() -> str:
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from serial_ports import resolve  # noqa: E402
    return resolve("m5stick")


class T1Session:
    """Full raw-wire T=1 session: setup -> IFS -> I-blocks (WTX-tolerant).

    Owns the CCID sequence byte (1..255 wrap) and the T=1 half-duplex
    sequence state (ns toggles per completed I-block exchange, nr tracks
    the reader's last I-block ns).
    """

    def __init__(self, port: str, stopbits: int = 2, baud: int = 115200,
                 settle: float = 1.0, window: float = 15.0,
                 verbose: bool = True, open_delay: float = 0.5):
        self.port = port
        self.stopbits = stopbits
        self.baud = baud
        self.settle = settle
        self.window = window
        self.verbose = verbose
        self.open_delay = open_delay
        self.seq = 0
        self.ns = 0
        self.nr = 0
        self.ifsc = 32  # t1.rs send_ifsc default; raised by negotiate_ifs
        self.s = None

    # -- port lifecycle ----------------------------------------------------
    def open(self) -> None:
        gempc_smoke = _import_labgrid_module("gempc_smoke")
        self.s = gempc_smoke.open_safe(self.port, stopbits=self.stopbits,
                                       baud=self.baud)
        time.sleep(self.open_delay)

    def close(self) -> None:
        if self.s is not None:
            try:
                self.s.close()
            finally:
                self.s = None

    def __enter__(self) -> T1Session:
        self.open()
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    # -- exchange plumbing ---------------------------------------------------
    def _next_seq(self) -> int:
        self.seq = (self.seq % 255) + 1
        return self.seq

    def _drain(self, window: float | None = None,
               settle: float | None = None) -> bytes:
        """Read until idle: 3 empty polls AFTER `settle` seconds, hard
        cap `window`. GemPC error paths legitimately take ~0.8 s and
        card ops seconds — fixed short windows misreport those as
        wedges (AGENTS.md reader-timing lesson)."""
        window = self.window if window is None else window
        settle = self.settle if settle is None else settle
        if self.s is None:
            raise T1Error("port not open")
        buf = b""
        t0 = time.time()
        idle = 0
        while time.time() - t0 < window:
            n = self.s.in_waiting
            if n:
                buf += self.s.read(n)
                idle = 0
                continue
            if time.time() - t0 >= settle:
                idle += 1
                if idle >= 3:
                    break
            time.sleep(0.05)
        return buf

    def exchange(self, msg_type: int, payload: bytes = b"", proto: int = 0,
                 window: float | None = None, settle: float | None = None,
                 label: str = "") -> tuple[int, bytes]:
        """Send one CCID frame, collect the raw reply window (echo +
        responses). Returns (seq used, raw bytes)."""
        seq = self._next_seq()
        frame = make_frame(msg_type, seq, payload, proto=proto)
        if self.s is None:
            raise T1Error("port not open")
        self.s.reset_input_buffer()
        self.s.write(frame)
        raw = self._drain(window, settle)
        if self.verbose:
            self._report(label or CCID_TYPE_NAMES.get(msg_type, hex(msg_type)),
                         msg_type, raw)
        return seq, raw

    def _report(self, label: str, msg_type: int, raw: bytes) -> None:
        echo, responses = split_echo(raw, msg_type)
        print(f"[{label}] {len(raw)} raw bytes, "
              f"echo={'yes' if echo else 'NO'}, "
              f"{len(responses)} response frame(s)")
        for f in responses:
            name = CCID_TYPE_NAMES.get(f.msg_type, hex(f.msg_type))
            line = (f"  {name} seq={f.seq} bStatus={f.bstatus:02X} "
                    f"plen={len(f.payload)}")
            if f.msg_type in (MSG_XFR_BLOCK, RDR_DATA_BLOCK) and f.payload:
                tb = parse_t1(f.payload)
                if tb is not None:
                    prev = tb.inf[:8].hex() + ("..." if len(tb.inf) > 8 else "")
                    line += f"  T=1 {tb.describe()} INF[:8]={prev}"
                else:
                    line += f"  payload={f.payload[:24].hex()}"
            print(line)

    # -- T=1 plumbing --------------------------------------------------------
    def xfr_block(self, block: bytes, label: str = "xfr",
                  window: float | None = None,
                  max_wtx: int = 6) -> tuple[bytes, T1Block | None]:
        """XfrBlock carrying one T=1 block; runs the S(WTX) dance.

        A deferred answer arrives as S(WTX request m) = [00 C3 01 m]+LRC;
        grant it with S(WTX response m) = [00 E3 01 m]+LRC in a further
        XfrBlock and the real answer follows. Returns the final answer
        block raw + parsed.
        """
        for _ in range(max_wtx):
            _seq, raw = self.exchange(MSG_XFR_BLOCK, block, window=window,
                                      label=label)
            answers = [f for f in parse_frames(raw)
                       if f.msg_type == RDR_DATA_BLOCK]
            if not answers:
                raise T1Error(f"{label}: no DataBlock response "
                              f"(wire: {raw[:64].hex()})")
            ans = answers[-1].payload
            tb = parse_t1(ans)
            if tb is not None and tb.is_wtx_request() and tb.complete:
                m = tb.inf[0] if tb.inf else 1
                if self.verbose:
                    print(f"  [{label}] S(WTX request m={m}) -> granting")
                block = wtx_response(m)
                continue
            return ans, tb
        raise T1Error(f"{label}: WTX dance did not converge "
                      f"after {max_wtx} grants")

    # -- session steps (the libccidtwin opening order) -----------------------
    def get_slot_status(self) -> Frame:
        _q, raw = self.exchange(MSG_GET_SLOT_STATUS, window=4.0,
                                label="GetSlotStatus")
        for f in parse_frames(raw):
            if f.msg_type == RDR_SLOT_STATUS:
                return f
        raise T1Error(f"GetSlotStatus: no SlotStatus reply "
                      f"(wire: {raw[:64].hex()})")

    def icc_power_on(self, attempts: int = 3) -> bytes:
        """IccPowerOn with the marginal-coupling retry pattern (power
        off + settle between attempts — t1_retry_probe)."""
        for i in range(attempts):
            _q, raw = self.exchange(MSG_ICC_POWER_ON, window=20.0,
                                    label=f"IccPowerOn#{i}")
            for f in parse_frames(raw):
                if f.msg_type == RDR_DATA_BLOCK and f.payload:
                    return bytes(f.payload)
            self.exchange(MSG_ICC_POWER_OFF, window=4.0,
                          label="IccPowerOff(retry)")
            time.sleep(1.0)
        raise T1Error("no ATR across IccPowerOn attempts (card coupling?)")

    def set_parameters_t1(self, params: bytes = SET_PARAMS_T1_PAYLOAD) -> Frame:
        """SetParameters with bProtocolNum=1 in the header tail — the
        step that ARMS the T=1 endpoint (see module docstring)."""
        _q, raw = self.exchange(MSG_SET_PARAMETERS, params, proto=1,
                                window=8.0, label="SetParameters(T=1)")
        for f in parse_frames(raw):
            if f.msg_type == RDR_PARAMETERS:
                return f
        raise T1Error(
            "SetParameters(T=1) not acknowledged (no RDR_to_PC_Parameters); "
            "if it later answers 6700/6E00 to T=1 XFRs, the header proto "
            f"byte was not 1 (wire: {raw[:64].hex()})")

    def negotiate_ifs(self, ifsc: int = DEFAULT_IFSC) -> int:
        """S(IFS request) XFR; the endpoint answers S(IFS response N) and
        raises its send limit to N. Without the EDC on the request the
        endpoint answers R(retransmit) and ifsc stays 32 — the EDC is
        part of the request, not an afterthought."""
        ans, tb = self.xfr_block(ifs_request(ifsc), label="IFS-request")
        if tb is None or not (tb.is_s and tb.s_response and tb.s_kind == "ifs"):
            raise T1Error(f"IFS negotiation failed: {ans.hex()} "
                          f"({tb.describe() if tb else 'unparseable'})")
        if not tb.edc_ok:
            raise T1Error(f"IFS response EDC BAD: {ans.hex()}")
        if tb.inf:
            self.ifsc = tb.inf[0]
        return self.ifsc

    def setup(self, negotiate: bool = True) -> bytes:
        """The libccidtwin opening order: GetSlotStatus -> IccPowerOn ->
        SetParameters(T=1) -> S(IFS request). Returns the ATR."""
        self.get_slot_status()
        atr = self.icc_power_on()
        self.set_parameters_t1()
        if negotiate:
            self.negotiate_ifs()
        return atr

    # -- APDU-level -----------------------------------------------------------
    def transceive(self, apdu: bytes, label: str = "apdu") -> bytes:
        """One APDU in a single I-block; returns the answer INF (SW tail
        included). APDUs longer than the negotiated IFSC must be chained
        manually with xfr_block (see t1_retry_probe for the pattern)."""
        if len(apdu) > self.ifsc:
            raise T1Error(f"APDU {len(apdu)}B exceeds IFSC {self.ifsc} — "
                          "chain I-blocks via xfr_block")
        block = i_block(self.ns, self.nr, apdu)
        ans, tb = self.xfr_block(block, label=label)
        if tb is None:
            raise T1Error(f"{label}: unparseable T=1 answer {ans.hex()}")
        if tb.is_s:
            raise T1Error(f"{label}: unexpected S-block {tb.describe()}")
        if tb.is_r:
            raise T1Error(f"{label}: R-block answer {tb.describe()} — "
                          "endpoint demanded retransmit or errored")
        if not tb.edc_ok:
            raise T1Error(f"{label}: answer EDC BAD {ans.hex()}")
        self.nr = (tb.ns + 1) & 1
        self.ns ^= 1
        # Codex P2 on #347: responses longer than the negotiated IFSC
        # arrive as a CHAIN — the first I-block carries M=1 and the
        # remainder only goes out after our R(ACK). Drain the chain
        # (bounded: a runaway chain cannot spin forever).
        inf = bytearray(tb.inf)
        guard = 0
        while tb.more:
            guard += 1
            if guard > 16:
                raise T1Error(f"{label}: response chain exceeded 16 "
                              "blocks — aborting drain")
            rack = r_block(self.nr)
            ans, tb = self.xfr_block(rack, label=f"{label}+RACK{guard}")
            if tb is None or not tb.is_i or not tb.edc_ok:
                raise T1Error(f"{label}: chain block bad "
                              f"({tb.describe() if tb else ans.hex()})")
            inf += tb.inf
        return bytes(inf)

    def select(self, aid: bytes) -> tuple[bytes, bytes]:
        """SELECT by AID (pure read, card-safety doctrine). Returns
        (answer INF, SW)."""
        apdu = bytes([0x00, 0xA4, 0x04, 0x00, len(aid)]) + aid
        inf = self.transceive(apdu, label=f"SELECT({aid[:6].hex()}..)")
        return inf, inf[-2:]


def run_probe(port: str, stopbits: int = 2, aid: bytes = DEFAULT_SELECT_AID,
              apdus: tuple[str, ...] = (), lock: bool = True,
              verbose: bool = True) -> int:
    """The --probe flow. Returns a process exit code."""
    esp32_flash = None
    lock_fd = None
    bench_fd = None
    if lock:
        esp32_flash = _import_labgrid_module("esp32_flash")
        # Codex P1 on #347: the pcscd flock prevents host interference
        # but does NOT reserve the DUT — take the bench flock first so
        # a concurrent HIL/flashing session cannot race the port.
        bench_fd = os.open("/tmp/amperstrand-bench.lock", os.O_RDWR)
        try:
            fcntl.flock(bench_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            os.close(bench_fd)
            bench_fd = None
            raise T1Error("bench flock held — another session owns the "
                          "DUT (labgrid place discipline, AGENTS.md)")
        lock_fd = esp32_flash.pcscd_lock_acquire()
    try:
        with T1Session(port, stopbits=stopbits, verbose=verbose) as sess:
            print(f"port={port} 115200 8N{stopbits}")
            atr = sess.setup()
            print(f"session armed: ATR ({len(atr)}B) {atr.hex()}, "
                  f"IFSC={sess.ifsc}")
            inf, sw = sess.select(aid)
            print(f"SELECT({aid.hex()}): answer {len(inf)}B "
                  f"SW={sw.hex()}")
            for hexapdu in apdus:
                apdu = bytes.fromhex(hexapdu)
                if is_sc_class_apdu(apdu):
                    raise T1Error(f"refusing SC-class APDU {hexapdu} "
                                  "(card-safety doctrine, AGENTS.md)")
                inf = sess.transceive(apdu, label=hexapdu)
                print(f"{hexapdu} -> {inf.hex()} SW={inf[-2:].hex()}")
    except T1Error as exc:
        print(f"PROBE FAIL: {exc}")
        return 1
    finally:
        if lock_fd is not None and esp32_flash is not None:
            esp32_flash.pcscd_lock_release(lock_fd)
        if bench_fd is not None:
            fcntl.flock(bench_fd, fcntl.LOCK_UN)
            os.close(bench_fd)
    return 0


def _cli() -> int:
    ap = argparse.ArgumentParser(
        description="Raw GemPC-Twin wire + T=1 session harness "
                    "(see GEMPC_WIRE_HARNESS.md)")
    ap.add_argument("--probe", action="store_true",
                    help="run GetSlotStatus/PowerOn/SetParams(T=1)/IFS/"
                         "SELECT and print every parsed frame")
    ap.add_argument("--port", default=None,
                    help="serial port (default: resolve m5stick by-id)")
    ap.add_argument("--stopbits", type=int, default=2,
                    help="2 = GemPC serial (default), 1 = USB-CDC")
    ap.add_argument("--aid", default=DEFAULT_SELECT_AID.hex(),
                    help="SELECT AID hex (default: bench SmartPGP applet)")
    ap.add_argument("--apdu", action="append", default=[],
                    metavar="HEX",
                    help="extra APDU to transceive after SELECT "
                         "(repeatable; SC-class APDUs are refused)")
    ap.add_argument("--no-pcscd-lock", action="store_true",
                    help="do not stop pcscd under the bench flock "
                         "(only when pcscd is already stopped)")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()
    if not args.probe:
        ap.print_help()
        return 2
    port = args.port or _default_port()
    return run_probe(port, stopbits=args.stopbits,
                     aid=bytes.fromhex(args.aid), apdus=tuple(args.apdu),
                     lock=not args.no_pcscd_lock, verbose=not args.quiet)


if __name__ == "__main__":
    raise SystemExit(_cli())
