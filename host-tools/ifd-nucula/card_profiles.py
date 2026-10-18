#!/usr/bin/env python3
"""Scripted card profiles for the mock board — picc emulation (research-cited).

Design transplanted from vsmartcard's vpicc (frankmorgner/vsmartcard,
`virtualsmartcard/src/vpicc/virtualsmartcard/VirtualSmartcard.py`): the
card emulator is a pluggable card OS behind the reader relay —
`VirtualICC.run()` dispatches each received C-APDU to
`SmartcardOS.execute(apdu)` and returns the R-APDU; power/ATR come from
`getATR()` / `powerUp()` / `powerDown()` / `reset()`
(vsmartcard docs, "Creating a Virtual Smart Card",
frankmorgner.github.io/vsmartcard/virtualsmartcard/api.html). Here the
"reader relay" is our GemPC mock board (mock_board.py): XfrBlock
payload -> CardOS.execute(), IccPowerOn -> CardOS ATR, exactly the
vpcd->vpicc split with the CCID-wire protocol the #157 IFD driver
speaks.

Profiles:

``legacy``        the historical mock: G-05 ATR, every APDU -> 90 00
                  (byte-identical pre-profile behavior; selftest/smoke
                  contract).
``satocash-g05``  the bench SatoCash card's #170 byte-level behavior:
                  5-byte case-2  B0 B0 00 00 00    -> 90 00 (good form,
                  ccid-pcsc-host case-form doctrine)
                  6-byte zero-data B0 B0 00 00 00 00 -> 67 00 (the card's
                  own length check rejects the bare Lc=0 before Le —
                  tests/hardware/nucula/vpcd_zerodata_repro.py, issue
                  #170; the #169 containment contract: the card answers
                  6700 and the SESSION stays usable)
                  SELECT by AID -> 90 00 (zero-risk set, bench doctrine)
``iso-case2``     a normal case-2 card: eID-class T=1 ATR, SELECT by AID
                  -> minimal FCI 6F 00 + 90 00, READ BINARY -> Le bytes
                  of deterministic (offset+i) data + 90 00, anything
                  else -> 6D 00 (INS not supported).

Usage: imported by mock_board.py (`--card <name>`); importable
standalone for unit tests (no socket machinery in this module).
"""
from __future__ import annotations

# G-05 ATR as synthesized by the firmware (pn7160-nci transport.rs test vector)
G05_ATR = bytes.fromhex("3B858131FE458073C8211084")
# ComSign eID T=1 ATR (bench contact card, AGENTS.md hardware history)
EID_ATR = bytes.fromhex("3BD518FF8191FE1FC38073C821100A")

SW_OK = b"\x90\x00"
SW_LEN_ERR = b"\x67\x00"  # wrong length (the #170 zero-data answer)
SW_INS_ERR = b"\x6d\x00"  # instruction not supported (ISO 7816-4 default)

# issue #170 bench-exact forms (vpcd_zerodata_repro.py)
CASE2_APDU = bytes([0xB0, 0xB0, 0x00, 0x00, 0x00])
ZERO_DATA_APDU = bytes([0xB0, 0xB0, 0x00, 0x00, 0x00, 0x00])
SELECT_BY_AID = bytes([0x00, 0xA4, 0x04, 0x00])


class CardOS:
    """Pluggable card OS — the vpicc SmartcardOS shape.

    The board relay calls these; subclass to script a card. Power hooks
    are lifecycle callbacks a subclass may use (vpicc has the same
    hooks); the board itself keeps presence semantics.
    """

    name = "card"

    def atr(self) -> bytes:
        raise NotImplementedError

    def execute(self, capdu: bytes) -> bytes:
        raise NotImplementedError

    def power_up(self) -> None:
        pass

    def power_down(self) -> None:
        pass

    def reset(self) -> None:
        pass


class StaticCard(CardOS):
    """One ATR + one fixed R-APDU for everything (the legacy mock)."""

    name = "legacy"

    def __init__(self, atr: bytes = G05_ATR, rapdu: bytes = SW_OK):
        self._atr = atr
        self.rapdu = rapdu

    def atr(self) -> bytes:
        return self._atr

    def execute(self, capdu: bytes) -> bytes:
        return self.rapdu


class RuleCard(CardOS):
    """ATR + ordered prefix-match rules, ISO fallthrough 6D00.

    Rules are tried FIRST-MATCH-WINS in list order — order matters when
    one APDU is a prefix of another (the #170 6-byte zero-data form must
    be matched before the 5-byte case-2 header). A responder is either
    bytes or a callable capdu->bytes.
    """

    def __init__(self, name: str, atr: bytes, rules, default: bytes = SW_INS_ERR):
        self.name = name
        self._atr = atr
        self.rules = list(rules)
        self.default = default

    def atr(self) -> bytes:
        return self._atr

    def execute(self, capdu: bytes) -> bytes:
        for prefix, respond in self.rules:
            if capdu.startswith(prefix):
                if callable(respond):
                    return respond(capdu)
                return respond
        return self.default


def _read_binary(capdu: bytes) -> bytes:
    """READ BINARY (00 B0 off1 off2 Le): Le bytes of (offset+i)&0xFF.

    Le = 0 means 256 (ISO 7816-4 case-2 short form). Deterministic,
    stateless — the tests assert exact bytes. A truncated header
    (< 5 bytes) answers 67 00 — the card's own length check, the same
    SW class as the #170 zero-data answer (pinned by test).
    """
    if len(capdu) < 5:
        return SW_LEN_ERR
    offset = ((capdu[2] & 0x7F) << 8) | capdu[3]
    le = capdu[4] if capdu[4] else 256
    data = bytes((offset + i) & 0xFF for i in range(le))
    return data + SW_OK


SATOCASH_G05 = RuleCard(
    name="satocash-g05",
    atr=G05_ATR,
    rules=[
        # order matters: the 6-byte zero-data form STARTS WITH the
        # 5-byte case-2 header — zero-data must be matched first
        (ZERO_DATA_APDU, SW_LEN_ERR),
        (CASE2_APDU, SW_OK),
        (SELECT_BY_AID, SW_OK),
    ],
)

ISO_CASE2 = RuleCard(
    name="iso-case2",
    atr=EID_ATR,
    rules=[
        (SELECT_BY_AID, b"\x6f\x00" + SW_OK),  # minimal FCI template
        (bytes([0x00, 0xB0]), _read_binary),
    ],
)

LEGACY = StaticCard()

PROFILES = {
    LEGACY.name: LEGACY,
    SATOCASH_G05.name: SATOCASH_G05,
    ISO_CASE2.name: ISO_CASE2,
}


def get_card(name: str) -> CardOS:
    try:
        return PROFILES[name]
    except KeyError:
        known = ", ".join(sorted(PROFILES))
        raise SystemExit(f"unknown card profile {name!r} (known: {known})") from None
