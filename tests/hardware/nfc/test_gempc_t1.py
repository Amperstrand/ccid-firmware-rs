"""Host unit tests for the gempc_t1 wire/T=1 harness (no hardware).

Run: python3 -m unittest tests/hardware/nfc/test_gempc_t1.py
(also collected by pytest). The harness battery is stdlib-only; the
cross-checks against conformance_battery (the canonical frame builder)
are skipped when pyserial is absent — that module imports it at module
scope and the CI python-collect gate runs without it.

GREAT EXPECTATION locks: byte sequences marked "bench-proven" were
proven on the m5stick GemPC wire 2026-10-20 (session scratch
t1_session.py / t1_verify.py). If one of these tests breaks because
the constant changed, re-verify against the bench — never "fix" the
test to match new unverified bytes.
"""

import functools
import importlib.util
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import gempc_t1  # noqa: E402

HAVE_SERIAL = importlib.util.find_spec("serial") is not None


def xor(data: bytes) -> int:
    return functools.reduce(lambda a, b: a ^ b, data, 0)


def hand_built_frame(msg_type: int, seq: int, payload: bytes,
                     proto: int = 0) -> bytes:
    """Independent builder — never the code under test."""
    body = (bytes([0x03, 0x06, msg_type])
            + len(payload).to_bytes(4, "little")
            + bytes([0, seq, proto, 0, 0])
            + payload)
    return body + bytes([xor(body)])


class FakeSerial:
    """Duck-typed serial port: echoes writes, emits scripted responses."""

    def __init__(self, responses: list[bytes]):
        self.responses = list(responses)
        self.writes: list[bytes] = []
        self._buf = b""

    def reset_input_buffer(self) -> None:
        self._buf = b""

    def write(self, data) -> None:
        data = bytes(data)
        self.writes.append(data)
        self._buf += data  # the device echo
        if self.responses:
            self._buf += self.responses.pop(0)

    @property
    def in_waiting(self) -> int:
        return len(self._buf)

    def read(self, n: int) -> bytes:
        out, self._buf = self._buf[:n], self._buf[n:]
        return out


class FrameBuildingTests(unittest.TestCase):
    def test_frame_xor_self_validates(self):
        f = gempc_t1.make_frame(0x65, 3, b"\x01\x02")
        self.assertEqual(xor(f), 0)

    def test_frame_shape_and_critical_proto_byte(self):
        # The 2026-10-20 session root cause: bProtocolNum must ride the
        # CCID header-tail byte index 3 (wire byte 9). Left at 0 the
        # T=1 endpoint never arms and XFRs go out as raw TPDUs.
        f = gempc_t1.make_frame(0x61, 1, gempc_t1.SET_PARAMS_T1_PAYLOAD,
                                proto=1)
        self.assertEqual(f[0], 0x03)
        self.assertEqual(f[1], 0x06)
        self.assertEqual(f[2], 0x61)
        self.assertEqual(int.from_bytes(f[3:7], "little"),
                         len(gempc_t1.SET_PARAMS_T1_PAYLOAD))
        self.assertEqual(f[7], 0)           # slot
        self.assertEqual(f[8], 1)           # seq
        self.assertEqual(f[9], 1)           # bBWI/bProtocolNum = T=1
        self.assertEqual(f[10], 0)
        self.assertEqual(f[11], 0)
        self.assertEqual(f[12:19], gempc_t1.SET_PARAMS_T1_PAYLOAD)
        self.assertEqual(f[19], xor(f[:19]))
        self.assertEqual(len(f), 20)

    def test_setparameters_frame_matches_hand_built(self):
        a = gempc_t1.make_frame(0x61, 7, gempc_t1.SET_PARAMS_T1_PAYLOAD,
                                proto=1)
        self.assertEqual(a, hand_built_frame(0x61, 7,
                                            gempc_t1.SET_PARAMS_T1_PAYLOAD,
                                            proto=1))

    def test_parse_roundtrip(self):
        f1 = gempc_t1.make_frame(0x65, 1)
        f2 = gempc_t1.make_frame(0x6F, 2, b"\x00\xc1\x01\xfe\x00")
        f3 = gempc_t1.make_frame(0x80, 2, b"\x3b\x85", proto=0)
        frames = gempc_t1.parse_frames(f1 + f2 + f3)
        self.assertEqual([(f.msg_type, f.seq, f.payload) for f in frames],
                         [(0x65, 1, b""),
                          (0x6F, 2, b"\x00\xc1\x01\xfe\x00"),
                          (0x80, 2, b"\x3b\x85")])
        self.assertEqual(frames[2].bstatus, 0)

    def test_parse_resyncs_past_console_noise(self):
        # shared CDC ports carry log text between frames — the parser
        # must resync (AGENTS.md reader-timing lesson)
        noise = b"ESP-ROM:esp32c3-api1-...\nrust main ALIVE\n"
        f = gempc_t1.make_frame(0x81, 4, b"", proto=0)
        self.assertEqual(len(gempc_t1.parse_frames(noise + f)), 1)

    def test_parse_skips_bad_lrc_but_finds_the_next(self):
        good = gempc_t1.make_frame(0x65, 1)
        bad = good[:-1] + bytes([good[-1] ^ 0xFF])
        self.assertEqual(gempc_t1.parse_frames(bad), [])
        self.assertEqual(len(gempc_t1.parse_frames(bad + good)), 1)

    def test_split_echo_separates(self):
        sent = gempc_t1.make_frame(0x6F, 9, gempc_t1.ifs_request())
        reply = gempc_t1.make_frame(0x80, 9, gempc_t1.t1_block(0xE1, b"\xfe"))
        echo, responses = gempc_t1.split_echo(sent + reply, 0x6F)
        self.assertIsNotNone(echo)
        self.assertEqual(echo.payload, gempc_t1.ifs_request())
        self.assertEqual([r.msg_type for r in responses], [0x80])

    def test_seq_counter_wraps_at_255(self):
        sess = gempc_t1.T1Session("/dev/null", verbose=False)
        seqs = [sess._next_seq() for _ in range(256)]
        self.assertEqual(seqs[0], 1)
        self.assertEqual(seqs[254], 255)
        self.assertEqual(seqs[255], 1)

    def test_sc_class_apdus_are_recognized(self):
        # AGENTS.md safe/unsafe table: INIT_UPDATE, EXT_AUTH and the
        # SC-gated GET DATA 00E0 are budget-bearing — never loopable
        for bad in ("80500000", "84820000", "00E00000"):
            self.assertTrue(gempc_t1.is_sc_class_apdu(bytes.fromhex(bad)),
                            bad)
        for ok in ("00A40400", "00CA006E", "80CA9F7F"):
            self.assertFalse(gempc_t1.is_sc_class_apdu(bytes.fromhex(ok)),
                             ok)


class T1BlockTests(unittest.TestCase):
    def test_ifs_request_bytes_bench_proven(self):
        # GREAT EXPECTATION: libccidtwin opens T=1 with exactly
        # 00 C1 01 FE + LRC (bench 2026-10-20; without the EDC the
        # endpoint answers R(retransmit) and send_ifsc stays 32).
        want = bytes([0x00, 0xC1, 0x01, 0xFE])
        self.assertEqual(gempc_t1.ifs_request()[:4], want)
        self.assertEqual(gempc_t1.ifs_request()[4], xor(want))

    def test_wtx_response_bytes_bench_proven(self):
        # S(WTX request m)=[00 C3 01 m]+LRC is granted with
        # [00 E3 01 m]+LRC (bench 2026-10-20, t1_retry_probe G-04).
        blk = gempc_t1.wtx_response(5)
        self.assertEqual(blk[:4], bytes([0x00, 0xE3, 0x01, 0x05]))
        self.assertEqual(blk[4], xor(blk[:4]))

    def test_i_block_select_bytes_bench_proven(self):
        # bench-proven SELECT I-block (ns=0): NAD 0 PCB 0 LEN 0x10 + AID
        aid = bytes.fromhex("D276000124010304AFAF000000000000")
        blk = gempc_t1.i_block(0, 0, aid)
        self.assertEqual(blk[:3], bytes([0x00, 0x00, len(aid)]))
        self.assertEqual(blk[3:3 + len(aid)], aid)
        self.assertEqual(blk[-1], xor(blk[:-1]))

    def test_i_block_pcb_layout(self):
        # mirrors firmware t1.rs i_pcb (ISO 7816-3 §11.4.2 Table 45)
        self.assertEqual(gempc_t1.i_pcb(0, False, 0), 0x00)
        self.assertEqual(gempc_t1.i_pcb(1, False, 0), 0x40)  # GET DATA ns=1
        self.assertEqual(gempc_t1.i_pcb(1, True, 0), 0x60)   # chained piece
        self.assertEqual(gempc_t1.i_pcb(0, False, 1), 0x10)

    def test_parse_roundtrip_and_fields(self):
        apdu = bytes.fromhex("00CA006E")
        blk = gempc_t1.i_block(1, 0, apdu)
        tb = gempc_t1.parse_t1(blk)
        self.assertIsNotNone(tb)
        assert tb is not None  # for the type checker
        self.assertTrue(tb.is_i)
        self.assertEqual(tb.ns, 1)
        self.assertFalse(tb.more)
        self.assertEqual(tb.nr, 0)
        self.assertEqual(tb.inf, apdu)
        self.assertTrue(tb.complete)
        self.assertTrue(tb.edc_ok)

    def test_edc_detects_corruption(self):
        blk = bytearray(gempc_t1.i_block(0, 0, b"\x6a\x82"))
        blk[-1] ^= 0xFF
        tb = gempc_t1.parse_t1(bytes(blk))
        self.assertFalse(tb.edc_ok)

    def test_short_block_returns_none(self):
        self.assertIsNone(gempc_t1.parse_t1(b"\x00\xc1\x01"))

    def test_truncated_block_is_incomplete(self):
        blk = gempc_t1.i_block(0, 0, b"\x6a\x82")
        tb = gempc_t1.parse_t1(blk[:-2])
        self.assertFalse(tb.complete)
        self.assertIsNone(tb.edc)
        self.assertFalse(tb.edc_ok)

    def test_wtx_request_classification(self):
        tb = gempc_t1.parse_t1(gempc_t1.t1_block(0xC3, b"\x05"))
        self.assertTrue(tb.is_s)
        self.assertTrue(tb.is_wtx_request())
        self.assertEqual(tb.s_kind, "wtx")
        self.assertFalse(tb.s_response)

    def test_s_response_classification(self):
        # response = request | 0x20 (t1.rs s_pcb, proto-t1.c:806):
        # WTX C3->E3, IFS C1->E1
        wtx = gempc_t1.parse_t1(gempc_t1.wtx_response(5))
        self.assertEqual(wtx.pcb, 0xE3)
        self.assertTrue(wtx.s_response)
        self.assertFalse(wtx.is_wtx_request())
        ifs = gempc_t1.parse_t1(gempc_t1.t1_block(0xE1, b"\xfe"))
        self.assertEqual((ifs.s_kind, ifs.s_response), ("ifs", True))

    def test_r_block_layout(self):
        blk = gempc_t1.r_block(1)
        self.assertEqual(blk[1], 0x90)
        tb = gempc_t1.parse_t1(blk)
        self.assertTrue(tb.is_r)
        self.assertEqual(tb.nr, 1)
        self.assertEqual(tb.r_err, 0)
        self.assertTrue(tb.edc_ok)

    def test_inf_over_ifsc_ceiling_rejected(self):
        with self.assertRaises(ValueError):
            gempc_t1.t1_block(0x00, b"\x00" * (gempc_t1.MAX_IFSC + 1))

    def test_describe_carries_edc_verdict(self):
        good = gempc_t1.parse_t1(gempc_t1.i_block(0, 0, b"\x61\x82"))
        self.assertIn("OK", good.describe())
        bad_blk = bytearray(gempc_t1.i_block(0, 0, b"\x61\x82"))
        bad_blk[-1] ^= 0x01
        self.assertIn("BAD", gempc_t1.parse_t1(bytes(bad_blk)).describe())


class SessionLogicTests(unittest.TestCase):
    """WTX dance + session state machine against a scripted port."""

    def _sess(self, responses: list[bytes]) -> gempc_t1.T1Session:
        sess = gempc_t1.T1Session("/dev/null", settle=0.0, window=1.0,
                                  verbose=False)
        sess.s = FakeSerial(responses)
        return sess

    def test_transceive_runs_the_wtx_dance(self):
        wtx_req = gempc_t1.t1_block(0xC3, b"\x05")
        answer = gempc_t1.i_block(0, 0, b"\x6a\x82")
        sess = self._sess([
            gempc_t1.make_frame(0x80, 2, wtx_req),
            gempc_t1.make_frame(0x80, 3, answer),
        ])
        sel = bytes.fromhex("00A40400023132")
        inf = sess.transceive(sel, label="sel")
        self.assertEqual(inf, b"\x6a\x82")
        self.assertEqual(sess.ns, 1)   # toggled after the completed chain
        self.assertEqual(sess.nr, 1)   # expecting the reader's ns=1
        xfrs = [w for w in sess.s.writes if w[2] == gempc_t1.MSG_XFR_BLOCK]
        self.assertEqual(len(xfrs), 2)
        self.assertEqual(xfrs[1][12:-1], gempc_t1.wtx_response(5))

    def test_transceive_plain_answer(self):
        answer = gempc_t1.i_block(0, 0, bytes.fromhex("02233132" + "9000"))
        sess = self._sess([gempc_t1.make_frame(0x80, 2, answer)])
        inf = sess.transceive(bytes.fromhex("00CA006E"), label="gd")
        self.assertEqual(inf[-2:], b"\x90\x00")

    def test_transceive_r_block_raises(self):
        sess = self._sess([
            gempc_t1.make_frame(0x80, 2, gempc_t1.r_block(0, 0))])
        with self.assertRaises(gempc_t1.T1Error):
            sess.transceive(bytes.fromhex("00CA006E"), label="gd")

    def test_transceive_bad_edc_raises(self):
        ans = bytearray(gempc_t1.i_block(0, 0, b"\x6a\x82"))
        ans[-1] ^= 0xFF
        sess = self._sess([gempc_t1.make_frame(0x80, 2, bytes(ans))])
        with self.assertRaises(gempc_t1.T1Error):
            sess.transceive(bytes.fromhex("00CA006E"), label="gd")

    def test_full_setup_shape(self):
        # the libccidtwin opening order, scripted end to end
        atr = bytes.fromhex("3B8580018073C821100E")
        sess = self._sess([
            gempc_t1.make_frame(0x81, 1, b""),                    # GSS
            gempc_t1.make_frame(0x80, 2, atr),                    # ATR
            gempc_t1.make_frame(0x82, 3, b""),                    # SetParams
            gempc_t1.make_frame(0x80, 4,                          # IFS resp
                                gempc_t1.t1_block(0xE1, b"\xfe")),
            gempc_t1.make_frame(0x80, 5,
                                gempc_t1.i_block(0, 0, b"\x90\x00")),
        ])
        got_atr = sess.setup()
        self.assertEqual(got_atr, atr)
        self.assertEqual(sess.ifsc, 254)
        inf, sw = sess.select(gempc_t1.DEFAULT_SELECT_AID)
        self.assertEqual(sw, b"\x90\x00")
        # setup order on the wire: GSS, PowerOn, SetParams(proto=1), IFS
        types = [w[2] for w in sess.s.writes]
        self.assertEqual(types[:4],
                         [0x65, 0x62, gempc_t1.MSG_SET_PARAMETERS,
                          gempc_t1.MSG_XFR_BLOCK])
        setparams = sess.s.writes[2]
        self.assertEqual(setparams[9], 1)  # header proto byte = T=1
        self.assertEqual(setparams[12:-1], gempc_t1.SET_PARAMS_T1_PAYLOAD)

    def test_negotiate_ifs_rejects_r_block_answer(self):
        # the no-EDC failure mode: endpoint answers R(retransmit)
        sess = self._sess([
            gempc_t1.make_frame(0x80, 2, gempc_t1.r_block(0, 0))])
        with self.assertRaises(gempc_t1.T1Error):
            sess.negotiate_ifs()


@unittest.skipUnless(HAVE_SERIAL, "pyserial not installed")
class CrossCheckConformanceBattery(unittest.TestCase):
    """DRY lock: gempc_t1's stdlib-only framing must stay byte-identical
    to conformance_battery's (the canonical, pyserial-scoped builder)."""

    def test_make_frame_matches_battery_frame(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from nfc.conformance_battery import frame as cb_frame
        samples = [
            (0x65, 1, b""),
            (0x6F, 7, b"\x00\xc1\x01\xfe"),
            (0x62, 200, b"\x01" * 40),
        ]
        for mt, seq, payload in samples:
            self.assertEqual(gempc_t1.make_frame(mt, seq, payload),
                             cb_frame(mt, seq, payload),
                             f"frame mismatch for type {mt:#x}")

    def test_parse_frames_agree(self):
        from conformance_battery import parse_frames as cb_parse
        buf = (b"log noise\n"
               + gempc_t1.make_frame(0x65, 1)
               + gempc_t1.make_frame(0x80, 1, b"\x3b\x85"))
        self.assertEqual(list(gempc_t1.parse_frames(buf)),
                         list(cb_parse(buf)))


if __name__ == "__main__":
    unittest.main()


class TestCodex347Followups:
    def test_gp_get_data_tag_in_p1p2_is_refused(self):
        # Codex P1 on #347: GP GET DATA carries the SC-gated tag in
        # P1/P2 (CLA/INS 80 CA or 84 CA + 00 E0) — a CLA-prefix check
        # misses it.
        from nfc.gempc_t1 import is_sc_class_apdu
        assert is_sc_class_apdu(bytes.fromhex("80CA00E000"))
        assert is_sc_class_apdu(bytes.fromhex("84CA00E000"))
        assert is_sc_class_apdu(bytes.fromhex("80CA9F7F00")) is False
        assert is_sc_class_apdu(bytes.fromhex("8050000000"))
        assert is_sc_class_apdu(bytes.fromhex("00A4040010")) is False

    def test_r_block_shape(self):
        from nfc.gempc_t1 import r_block, lrc
        b = r_block(1)
        assert b[1] == 0x90 and b[2] == 0x00 and b[-1] == lrc(b[:-1])

    def test_logical_channel_clas_normalized(self):
        # Codex P1 on #347: GP commands on supplementary logical
        # channels carry the channel in CLA b1..b3 (81 50, 85 82, 81 CA)
        # — the guard must normalize before comparing.
        from nfc.gempc_t1 import is_sc_class_apdu
        assert is_sc_class_apdu(bytes.fromhex("8150000000"))
        assert is_sc_class_apdu(bytes.fromhex("8582000000"))
        assert is_sc_class_apdu(bytes.fromhex("81CA00E000"))
        assert is_sc_class_apdu(bytes.fromhex("00CA006E")) is False

    def test_trailing_bytes_rejected(self):
        # Codex P2 on #347: a valid block + trailing garbage must not
        # parse as complete.
        from nfc.gempc_t1 import parse_t1, i_block
        good = i_block(0, 0, bytes.fromhex("00CA006E"))
        tb = parse_t1(good + b"\xff")
        assert tb is not None and not tb.complete

    def test_corrupt_wtx_not_granted(self):
        # Codex P2 on #347: a bad-EDC WTX request must raise, not grant.
        import pytest
        from nfc import gempc_t1
        from nfc.gempc_t1 import T1Error, T1Session, MSG_XFR_BLOCK, Frame

        # outer GemPC frame built with a correct LRC so parse_frames
        # accepts it; the INNER T=1 block carries the wrong EC.
        inner = bytes([0x00, 0xC3, 0x01, 0x02, 0x00])  # EDC 00; correct = C0
        body = (bytes([0x03, 0x06, 0x80]) + len(inner).to_bytes(4, "little")
                + bytes([0x00, 0x01, 0x00, 0x00, 0x00]) + inner)
        wire = body + bytes([gempc_t1.lrc(body)])

        class FakeSess:
            verbose = False
            def exchange(self, *a, **k):
                return 0, wire

        with pytest.raises(T1Error, match="BAD EDC"):
            T1Session.xfr_block(FakeSess(), b"\x00\x00\x00\x00\x00",
                                label="t")
