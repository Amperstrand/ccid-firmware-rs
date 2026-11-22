#!/usr/bin/env python3
"""gempc_t1 — the python twin of the firmware's T=1 endpoint.

Faithful port of crates/ccid-service/src/t1.rs (the #101-hardened,
bench-proven endpoint) for the gempc_sim virtual card: pcscd's internal
T=1 stack speaks BLOCKS through XfrBlock (libccidtwin is TPDU-level),
so the card side must answer blocks exactly like the firmware does.

Wire contract (ISO 7816-3 §11, quote-locked in t1.rs):
  block   = NAD(1) | PCB(1) | LEN(1) | INF(LEN) | LRC(1)   (§11.3)
  I-block PCB = 0 N(S) M N(R) 0 0 0 0    (N(S)/N(R) at 0x40/0x10)
  R-block PCB = 1 0 N(R) 0 0 err err     (0x80 | nr<<4 | err)
  S-block PCB = 1 1 [resp 0x20] kind     (C0 resync C1 ifs C2 abort C3 wtx)
Behavioral rules ported verbatim: chain-piece ACK R(ns,0) (§11.6.3);
R-block → retransmit last / next chain block (§11.5.3.3); #101 retry
idempotence (duplicate final I-block replays the cached first frame,
duplicate chain piece re-ACKs, never re-appends); RESYNC resets both
counters; IFSD request bounds our sends; sequence violation → R(0).
The sim's card answers instantly, so WE never request WTX (the
firmware's BWT-derived m is relay territory); host WTX requests get
the §11-conformant response echo.
"""
from __future__ import annotations

MAX_IFSC = 254
MAX_RELAY = 512

RESYNC, IFS, ABORT, WTX = 0x00, 0x01, 0x02, 0x03


def lrc(data: bytes) -> int:
    x = 0
    for b in data:
        x ^= b
    return x


def block(pcb: int, inf: bytes = b"") -> bytes:
    head = bytes([0x00, pcb, len(inf)]) + inf
    return head + bytes([lrc(head)])


def i_pcb(ns: int, more: bool, nr: int) -> int:
    return (ns << 6) | ((1 if more else 0) << 5) | (nr << 4)


def r_pcb(nr: int, err: int = 0) -> int:
    return 0x80 | (nr << 4) | err


def s_pcb(kind: int, response: bool) -> int:
    return (0xC0 | kind) | (0x20 if response else 0)


def looks_like_block(data: bytes) -> bool:
    """Structural T=1 test: LEN must cover the payload exactly and the
    LRC must validate. Used to dispatch XfrBlock payloads between the
    T=1 endpoint and raw-APDU consumers (direct-serial tooling)."""
    return (len(data) >= 4 and data[2] == len(data) - 4
            and lrc(data[:-1]) == data[-1])


class T1Endpoint:
    """Card-side endpoint. feed(block) -> ('imm', bytes) to return
    immediately, or ('apdu', bytes) assembled APDU to answer via
    card_response(). Port of t1.rs T1Endpoint."""

    def __init__(self, ifsc: int = MAX_IFSC):
        self.ns = 0            # our send sequence
        self.host_ns = 0       # next expected host N(S)
        self.recv_chain: list[int] = []
        self.send_chain: bytes = b""
        self.send_ifsc = ifsc  # bounded by host IFSD requests
        self.last_sent: bytes | None = None
        self.retry_ns: int | None = None
        self.retry_first_frame: bytes | None = None

    # -- host block in ------------------------------------------------
    def feed(self, data: bytes):
        if len(data) < 4:
            return self._bad()
        pcb, ln = data[1], data[2]
        if len(data) != 3 + ln + 1 or ln > MAX_IFSC:
            return self._bad()
        if lrc(data[:3 + ln]) != data[3 + ln]:
            return self._bad()
        inf = data[3:3 + ln]

        if pcb & 0xC0 == 0xC0:  # S-block
            return self._s_request(pcb, inf) if not pcb & 0x20 \
                else ("imm", b"")  # S responses need no card answer
        if pcb & 0xC0 == 0x80:  # R-block: retransmit (§11.5.3.3)
            nxt = self._take_next_chain_block()
            if nxt is not None:
                return ("imm", nxt)
            if self.last_sent is not None:
                return ("imm", self.last_sent)
            return self._bad()
        # I-block
        ns, more = (pcb >> 6) & 1, bool(pcb & 0x20)
        if ns != self.host_ns:
            if ns == self.retry_ns:  # #101 duplicate handling
                if more:
                    return ("imm", block(r_pcb(ns, 0)))  # re-ACK piece
                if self.retry_first_frame is not None:
                    return ("imm", self.retry_first_frame)
            return ("imm", block(r_pcb(self.host_ns, 0)))  # §11.5.3
        self.recv_chain += inf
        if more:
            return ("imm", block(r_pcb(ns, 0)))  # §11.6.3 chain ACK
        self.host_ns ^= 1
        apdu, self.recv_chain = bytes(self.recv_chain), []
        return ("apdu", apdu)

    def _s_request(self, pcb: int, inf: bytes):
        kind = pcb & 0x03
        if kind == RESYNC:
            self.ns = self.host_ns = 0
            self.recv_chain = []
            self.send_chain = b""
            self.retry_ns = self.retry_first_frame = None
            rsp = block(s_pcb(RESYNC, True))
        elif kind == IFS:
            self.send_ifsc = min(inf[0] if inf else 32, MAX_IFSC)
            rsp = block(s_pcb(IFS, True), inf)
        elif kind == ABORT:
            self.recv_chain = []
            rsp = block(s_pcb(ABORT, True), inf)
        else:  # WTX: host-requested — answer with the §-conformant echo
            rsp = block(s_pcb(WTX, True), inf)
        self.last_sent = rsp
        return ("imm", rsp)

    # -- card answer out ----------------------------------------------
    def card_response(self, rsp: bytes) -> bytes:
        self.send_chain = rsp[:MAX_RELAY]
        first = self._take_next_chain_block()
        if first is None:
            first = block(i_pcb(self.ns, False, self.host_ns))
            self.ns ^= 1
            self.last_sent = first
        self.retry_ns = self.host_ns ^ 1
        self.retry_first_frame = first
        return first

    def _take_next_chain_block(self) -> bytes | None:
        if not self.send_chain:
            return None
        chunk, self.send_chain = (self.send_chain[:self.send_ifsc],
                                  self.send_chain[self.send_ifsc:])
        more = bool(self.send_chain)
        b = block(i_pcb(self.ns, more, self.host_ns), chunk)
        if not more:
            self.ns ^= 1
        self.last_sent = b
        return b

    def _bad(self):
        return ("imm", block(r_pcb(self.host_ns, 1)))  # R(ns, parity err)
