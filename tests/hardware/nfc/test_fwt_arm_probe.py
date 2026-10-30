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
# which imports pyserial — stub it when absent.
try:
    import serial  # noqa: F401, E402
except ModuleNotFoundError:
    import types as _types

    sys.modules["serial"] = _types.ModuleType("serial")

import fwt_arm_probe as probe  # noqa: E402


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
    def test_bare_payload(self):
        self.assertEqual(
            probe.parse_d3_payload(struct.pack("<III", 1520, 32, 2483)),
            (1520, 32, 2483))

    def test_rdr_headed_payload(self):
        headed = b"\x81" + b"\x00" * 9 + struct.pack("<III", 1, 2, 3)
        self.assertEqual(probe.parse_d3_payload(headed), (1, 2, 3))

    def test_short_is_none(self):
        self.assertIsNone(probe.parse_d3_payload(b"\x00" * 8))


class VerdictTests(unittest.TestCase):
    def test_1638_clamp_is_pre_fix(self):
        self.assertEqual(probe._verdict(1638, 13), 2)
        self.assertEqual(probe._verdict(1638, 14), 2)

    def test_full_fwt_is_fixed(self):
        self.assertEqual(probe._verdict(2475, 13), 0)
        self.assertEqual(probe._verdict(4949, 14), 0)
        self.assertEqual(probe._verdict(4957, 14), 0)  # bolty's figure

    def test_none_is_pre_fix(self):
        self.assertEqual(probe._verdict(None, 13), 2)


class FeatureTlvTests(unittest.TestCase):
    def _tlv(self, *pairs):
        out = b""
        for tag, val in pairs:
            out += bytes([tag, 4]) + struct.pack("<I", val)
        return out

    def test_finds_esc_tag_after_others(self):
        tlv = self._tlv((2, 0x1000001), (19, 0x1000042))
        self.assertEqual(probe._feature_ioctl(tlv, 19), 0x1000042)

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
