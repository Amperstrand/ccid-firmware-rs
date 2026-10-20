"""Host unit tests for the gempc_smoke boot-smoke probe (no hardware).

Run: python3 -m unittest tests/hardware/labgrid/test_gempc_smoke.py
(also collected by pytest; serial-dependent behavior is skipped when
pyserial is absent — the framing battery imports it at module scope).
"""

import importlib.util
import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import gempc_smoke  # noqa: E402

HAVE_SERIAL = importlib.util.find_spec("serial") is not None


def frame_bytes(msg_type: int, seq: int, bstatus: int, payload: bytes,
                lrc_ok: bool = True) -> bytes:
    """Hand-built RDR frame (independent of the code under test)."""
    header = (bytes([msg_type])
              + len(payload).to_bytes(4, "little")
              + bytes([0, seq, bstatus, 0, 0]))
    body = bytes([gempc_smoke.SYNC, gempc_smoke.CTRL_ACK]) + header + payload
    lrc = 0
    for b in body:
        lrc ^= b
    if not lrc_ok:
        lrc ^= 0xFF
    return body + bytes([lrc])


@unittest.skipUnless(HAVE_SERIAL, "pyserial not installed")
class FramingTests(unittest.TestCase):
    def test_make_frame_lrc_self_validates(self):
        # the built frame must parse as a valid RDR-less stream: its
        # LRC makes the whole frame XOR to zero
        f = gempc_smoke.make_frame(gempc_smoke.MSG_GET_SLOT_STATUS)
        x = 0
        for b in f:
            x ^= b
        self.assertEqual(x, 0)

    def test_wire_reply_ok_accepts_valid_rdr_frame(self):
        # the #294 boot-smoke gate: an LRC-valid reply proves serving
        self.assertTrue(gempc_smoke.wire_reply_ok(
            frame_bytes(0x81, 0, 0x02, b"")))

    def test_wire_reply_ok_rejects_lrc_corrupt_frame(self):
        self.assertFalse(gempc_smoke.wire_reply_ok(
            frame_bytes(0x81, 0, 0x02, b"", lrc_ok=False)))

    def test_wire_reply_ok_rejects_empty(self):
        # no evidence is not proof (#299 class)
        self.assertFalse(gempc_smoke.wire_reply_ok(b""))

    def test_wire_reply_ok_resyncs_past_console_noise(self):
        # shared CDC ports carry log text between frames — the parser
        # must resync (console-none c3 prints ROM banner + logs)
        noise = b"ESP-ROM:esp32c3-api1-...\nrust main ALIVE\n"
        self.assertTrue(gempc_smoke.wire_reply_ok(
            noise + frame_bytes(0x81, 0, 0x02, b"")))


@unittest.skipUnless(HAVE_SERIAL, "pyserial not installed")
class DiagnosticsTests(unittest.TestCase):
    def _diag(self, apdu_tx=5, apdu_rx=7, nak=1, err=0, reinit=2,
              present=1, uptime=34096) -> bytes:
        return b"".join(struct.pack("<I", v) for v in
                        (apdu_tx, apdu_rx, nak, err, reinit,
                         present, uptime))

    def test_parse_frozen_layout(self):
        # byte-exact frozen 0xD0 format (ccid-core diagnostics golden)
        d = gempc_smoke.parse_diagnostics(self._diag())
        self.assertIsNotNone(d)
        assert d is not None  # for the type checker
        self.assertEqual(d["uptime_ticks"], 34096)
        self.assertEqual(d["apdu_tx"], 5)
        self.assertTrue(d["card_present"])

    def test_short_payload_is_rejected_not_guessed(self):
        # never guess offsets on a short buffer — a wrong-field uptime
        # would silently pass liveness on garbage
        self.assertIsNone(gempc_smoke.parse_diagnostics(self._diag()[:24]))
        self.assertIsNone(gempc_smoke.parse_diagnostics(b""))

    def test_long_payload_uses_first_28(self):
        d = gempc_smoke.parse_diagnostics(self._diag() + b"\xff" * 8)
        self.assertIsNotNone(d)
        assert d is not None  # for the type checker
        self.assertEqual(d["uptime_ticks"], 34096)

    def test_slot_status_and_atr_selectors(self):
        frames = [
            (0x81, 0, 0x02, 0, b""),           # SlotStatus, icc=2, cmd=0
            (0x80, 1, 0x00, 0, b"\x3b\x85\x81"),
        ]
        self.assertEqual(gempc_smoke.slot_status_of(frames), (2, 0))
        self.assertEqual(gempc_smoke.atr_of(frames), b"\x3b\x85\x81")

    def test_selectors_ignore_other_types(self):
        self.assertIsNone(gempc_smoke.slot_status_of(
            [(0x80, 0, 0, 0, b"\x3b")]))
        self.assertIsNone(gempc_smoke.atr_of(
            [(0x81, 0, 0, 0, b"")]))


if __name__ == "__main__":
    unittest.main()
