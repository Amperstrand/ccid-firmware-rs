"""Host unit tests for fwt_arm_probe.py — the #180 on-wire fingerprint.

Locks the probe's verdict arithmetic (the 1638 ms clamp fingerprint vs
the FWI-derived full FWT), the ATR BWI parser, the 0xD3 payload parse,
and the Part10 feature-TLV walk — no hardware required; the bench run
only proves plumbing against the live reader.

Run: python3 -m unittest tests/hardware/nfc/test_fwt_arm_probe.py
(also collected by pytest).
"""

import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))  # nfc/, the probe's dir

# python-collect minimal-env guard (test_conformance_battery.py pattern,
# #361 review P1): the probe imports conformance_battery at module level,
# which imports pyserial — stub it when absent, then DO NOT LEAK the stub
# (a bare ModuleType carries __spec__=None, which makes sibling modules'
# importlib.util.find_spec("serial") RAISE and breaks their collection —
# main went red exactly this way once: python-collect 2026-10-30).
_stubbed_serial = False
try:
    import serial  # noqa: F401, E402
except ModuleNotFoundError:
    import types as _types

    sys.modules["serial"] = _types.ModuleType("serial")
    _stubbed_serial = True

import fwt_arm_probe as probe  # noqa: E402

if _stubbed_serial:
    del sys.modules["serial"]


class FwtFormulaTests(unittest.TestCase):
    def test_fwi13_14_full_fwts(self):
        # ISO 14443-3 §7.2: FWT = 256 x 16 / 13.56 MHz x 2^FWI.
        for fwi, want in [(13, 2474.5), (14, 4949.0)]:
            got = probe.fwt_ms_for_fwi(fwi)
            self.assertLess(abs(got - want) / want, 0.01,
                            f"FWI {fwi}: {got} vs {want}")

    def test_pre_fix_ceiling_is_below_fwi13(self):
        # The #180 fingerprint: the pre-fix timer's 1638 ms ceiling sits
        # under every FWI >= 13 FWT — the verdict must catch that gap.
        self.assertLess(1638, probe.fwt_ms_for_fwi(13) * 0.99)


class AtrBwiTests(unittest.TestCase):
    def test_tb1_carrying_atr(self):
        # Synthetic contact T=1 ATR with TB1=0xB1 (T0=0xB5: TB1+TD1
        # present) -> BWI = 11.
        atr = bytes.fromhex("3BB511B1016000")  # TS T0 TA1 TB1 TD1 hist TCK
        self.assertEqual(probe.fwi_from_atr(atr), 11)

    def test_nfc_atr_has_no_tb1(self):
        # The bench nucula ATR: T0=0x85 (TD1 only) — the 0xB1 is a
        # HISTORICAL byte, not TB1. NFC FWI lives in the ATS TB(1),
        # which the synthesized ATR does not carry: None, and the card
        # leg must use --expect-fwi.
        atr = bytes.fromhex("3B85800180B1C821100E")
        self.assertIsNone(probe.fwi_from_atr(atr))

    def test_atr_without_interface_bytes(self):
        self.assertIsNone(probe.fwi_from_atr(bytes.fromhex("3B850180C8")))

    def test_garbage_is_none(self):
        self.assertIsNone(probe.fwi_from_atr(b"\x00"))
        self.assertIsNone(probe.fwi_from_atr(b""))


class D3PayloadTests(unittest.TestCase):
    def test_bare_legacy_payload(self):
        self.assertEqual(
            probe.parse_d3_payload(struct.pack("<III", 1520, 32, 2483)),
            (1520, 32, 2483, None))

    def test_bare_extended_payload_carries_regs(self):
        # #519: 12 legacy bytes + [TMode, TPrescaler, TReloadHi,
        # TReloadLo] — FWI-14-class arming (prescaler 0x201).
        raw = struct.pack("<III", 1520, 32, 4957) + bytes([0x82, 0x01,
                                                           0x4E, 0x20])
        self.assertEqual(probe.parse_d3_payload(raw),
                         (1520, 32, 4957, bytes([0x82, 0x01, 0x4E, 0x20])))

    def test_rdr_headed_payload(self):
        headed = b"\x81" + b"\x00" * 9 + struct.pack("<III", 1, 2, 3)
        self.assertEqual(probe.parse_d3_payload(headed), (1, 2, 3, None))

    def test_rdr_headed_extended_payload(self):
        headed = b"\x83" + b"\x00" * 9 + struct.pack("<III", 1, 2, 3) + \
            bytes([0x8A, 0xA9, 0x19, 0x9A])
        self.assertEqual(probe.parse_d3_payload(headed)[3],
                         bytes([0x8A, 0xA9, 0x19, 0x9A]))

    def test_short_is_none(self):
        self.assertIsNone(probe.parse_d3_payload(b"\x00" * 8))


class ReadbackArithmeticTests(unittest.TestCase):
    """#519: the authoritative armed period from the raw registers."""

    def test_pre_fix_fixed_0xa9_arming_caps_at_1638(self):
        # Fixed 0xA9 prescaler, reload 0xFFFF: 65535 x 339 / 13.56 MHz
        # ~= 1638 ms — the pre-#464 ceiling.
        armed = probe.armed_fwt_ms_from_regs(bytes([0x80, 0xA9, 0xFF, 0xFF]))
        self.assertLess(abs(armed - 1638.4) / 1638.4, 0.01)

    def test_post_fix_fwi14_arming_covers_4957(self):
        # prescaler 0x201 (divisor 1027), reload 0xFFFF: 65535 x 1027 /
        # 13.56 MHz ~= 4964 ms >= the FWI-14 FWT (4949 ms) — the
        # post-#464 per-request prescaler path.
        armed = probe.armed_fwt_ms_from_regs(bytes([0x82, 0x01, 0xFF, 0xFF]))
        self.assertGreaterEqual(armed, probe.fwt_ms_for_fwi(14) * 0.99)


class VerdictTests(unittest.TestCase):
    """rc 0 FIXED / 2 PRE-FIX / 3 INCONCLUSIVE (legacy payload)."""

    # --- readback present: authoritative ---
    def test_readback_1638_clamp_is_pre_fix(self):
        regs = bytes([0x80, 0xA9, 0xFF, 0xFF])  # ~1638 ms ceiling
        self.assertEqual(probe._verdict(4957, 14, regs), 2)
        self.assertEqual(probe._verdict(2483, 13, regs), 2)

    def test_readback_covering_is_fixed(self):
        regs = bytes([0x82, 0x01, 0xFF, 0xFF])  # ~4964 ms >= FWI-14 FWT
        self.assertEqual(probe._verdict(4957, 14, regs), 0)

    def test_cardless_idle_arming_is_inconclusive(self):
        # Bench-proven 2026-10-31: card-less, the fw holds the IDLE
        # arming (100 ms default, legitimately prescaler 0xA9 — every
        # sub-1638 ms timeout keeps the historical register bytes
        # post-fix). That is NOT a pre-fix fingerprint.
        idle = bytes([0x80, 0xA9, 0x0F, 0xA0])  # readback 80a90fa0
        self.assertEqual(probe._verdict(0, 14, idle, card_leg=False), 3)

    def test_readback_1638_for_fwi13_is_pre_fix(self):
        # 1638 ms < FWI-13 FWT (2475 ms) — the gap the verdict catches.
        regs = bytes([0x80, 0xA9, 0xFF, 0xFF])
        self.assertEqual(probe._verdict(2483, 13, regs), 2)

    # --- legacy payload: word 3 is the REQUEST cache — no verdict
    #     possible (the #519 false-pass class). ---
    def test_legacy_payload_is_inconclusive_even_when_word3_claims_fixed(self):
        self.assertEqual(probe._verdict(2475, 13, None), 3)
        self.assertEqual(probe._verdict(4957, 14, None), 3)

    def test_legacy_payload_inconclusive_even_with_clamp_word3(self):
        self.assertEqual(probe._verdict(1638, 14, None), 3)


class FeatureTlvTests(unittest.TestCase):
    def _tlv(self, *pairs):
        # PC/SC v2 Part 10 §2.2 (reader.h:164): value is BIG ENDIAN.
        out = b""
        for tag, val in pairs:
            out += bytes([tag, 4]) + struct.pack(">I", val)
        return out

    def test_finds_esc_tag_after_others(self):
        tlv = self._tlv((2, 0x1000001), (19, 0x1000042))
        self.assertEqual(probe._feature_ioctl(tlv, 19), 0x1000042)

    def test_real_shape_linux_esc_ioctl(self):
        # pcscd on Linux advertises ESC as SCARD_CTL_CODE(1) =
        # 0x42000001 — wire bytes 42 00 00 01 (BE). The pre-#519 LE
        # decode read this as 0x01000042 and every subsequent control
        # hit the wrong ioctl.
        tlv = self._tlv((19, 0x42000001))
        self.assertEqual(probe._feature_ioctl(tlv, 19), 0x42000001)

    def test_absent_tag(self):
        self.assertIsNone(probe._feature_ioctl(self._tlv((2, 1)), 19))

    def test_truncated_tlv(self):
        self.assertIsNone(probe._feature_ioctl(b"\x13\x08\x01", 19))


class EscapeMessageTests(unittest.TestCase):
    def test_header_shape(self):
        msg = probe.build_escape_msg(7, b"\xD3\x20")
        self.assertEqual(len(msg), 12)
        self.assertEqual(msg[0], 0x6B)  # PC_to_RDR_Escape
        self.assertEqual(msg[1:5], struct.pack("<I", 2))
        self.assertEqual(msg[5], 0)     # bSlot
        self.assertEqual(msg[6], 7)     # bSeq
        self.assertEqual(msg[-2:], b"\xD3\x20")


if __name__ == "__main__":
    unittest.main()
