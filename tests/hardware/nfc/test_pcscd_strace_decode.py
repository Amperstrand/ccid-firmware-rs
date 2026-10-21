"""Host unit tests for pcscd_strace_decode (no hardware).

Run: python3 -m unittest tests/hardware/nfc/test_pcscd_strace_decode.py
(also collected by pytest; stdlib only — the CI collect gate imports
this file in a minimal env).
"""

import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import gempc_t1  # noqa: E402
import pcscd_strace_decode as psd  # noqa: E402


def strace_quote(data: bytes) -> str:
    """Render bytes the way `strace -xx` does (printables literal,
    everything else as \\xNN; quote and backslash always escaped)."""
    out = []
    for b in data:
        if 32 <= b < 127 and b not in (0x22, 0x5C):
            out.append(chr(b))
        else:
            out.append(f"\\x{b:02x}")
    return '"' + "".join(out) + '"'


class UnescapeTests(unittest.TestCase):
    def test_hex_escapes(self):
        self.assertEqual(psd.unescape(r"\x03\x06\x41"), b"\x03\x06A")

    def test_printable_literal(self):
        self.assertEqual(psd.unescape("pcscd"), b"pcscd")

    def test_backslash_and_quote(self):
        self.assertEqual(psd.unescape(r'\\\"'), b'\x5c\x22')

    def test_letter_escapes_decode(self):
        # legacy (non -xx) capture forms — decoded, but flagged
        self.assertEqual(psd.unescape(r"\f\n\t\r\v"), b"\x0c\x0a\x09\x0d\x0b")

    def test_octal_escapes_decode(self):
        self.assertEqual(psd.unescape(r"\012\377\0"), b"\x0a\xff\x00")

    def test_legacy_count_zero_for_xx_capture(self):
        self.assertEqual(psd.legacy_escape_count(r"a\x01b\\c\"d"), 0)

    def test_legacy_count_flags_letter_and_octal(self):
        self.assertEqual(psd.legacy_escape_count(r"\f \012 \t"), 3)

    def test_legacy_count_ignores_escaped_backslash(self):
        # bytes 0x5C 'n' render as \\n — that is NOT a letter escape
        self.assertEqual(psd.legacy_escape_count(r"\\n\\f"), 0)


class LineParseTests(unittest.TestCase):
    def test_complete_write_line(self):
        frame = gempc_t1.make_frame(0x6F, 9, gempc_t1.ifs_request())
        line = f"12345  write(6, {strace_quote(frame)}, {len(frame)})" \
               f" = {len(frame)}\n"
        events = psd.parse_strace(line)
        self.assertEqual(len(events), 1)
        e = events[0]
        self.assertEqual((e.op, e.fd, e.data, e.ret),
                         ("write", 6, frame, len(frame)))

    def test_negative_return_skipped(self):
        line = '12345  read(6, "", 8192) = -1 EAGAIN (Resource ' \
               'temporarily unavailable)\n'
        self.assertEqual(psd.parse_strace(line), [])

    def test_pid_prefix_form(self):
        line = f'[pid 99] write(6, {strace_quote(b"AB")}, 2) = 2\n'
        events = psd.parse_strace(line)
        self.assertEqual(events[0].data, b"AB")

    def test_unfinished_resumed_read(self):
        # -f thread interleave: the read payload lands on the resumed line
        text = ('12345  read(6,  <unfinished ...>\n'
                '12346  futex(0x7f00, FUTEX_WAIT, 0, NULL) = 0\n'
                '12345  <... read resumed>"\\x03\\x06", 2) = 2\n')
        events = psd.parse_strace(text)
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0].op, "read")
        self.assertEqual(events[0].data, b"\x03\x06")

    def test_unfinished_write_data_survives_resume(self):
        # a write's payload prints on the unfinished line
        text = ('12345  write(6, "AB", 2  <unfinished ...>\n'
                '12345  <... write resumed>) = 2\n')
        events = psd.parse_strace(text)
        self.assertEqual([(e.op, e.data) for e in events],
                         [("write", b"AB")])

    def test_pin_fd_override(self):
        events = [psd.Syscall(1, "write", 6, b"x", 1)]
        fd, why = psd.pin_fd(events, fd_arg=6)
        self.assertEqual(fd, 6)
        self.assertIn("--fd", why)

    def test_is_serial_device(self):
        self.assertTrue(psd.is_serial_device(
            "/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-if00-port0"))
        self.assertTrue(psd.is_serial_device("/dev/ttyUSB0"))
        self.assertFalse(psd.is_serial_device("socket:[12345]"))
        self.assertFalse(psd.is_serial_device("/dev/null"))


class DecodeTests(unittest.TestCase):
    def test_read_stream_reassembly_across_split_reads(self):
        # one frame split across two read()s must decode as ONE frame
        reply = gempc_t1.make_frame(0x80, 9, gempc_t1.t1_block(0xE1, b"\xfe"))
        stream = reply[:5] + reply[5:]
        self.assertEqual(len(psd.parse_frames(stream)), 1)
        lines = psd.decode_stream(stream, "RX")
        self.assertTrue(any("1 LRC-valid frame" in ln for ln in lines))

    def test_decode_reports_t1_blocks_with_edc(self):
        tx = gempc_t1.make_frame(0x6F, 9, gempc_t1.ifs_request())
        lines = psd.decode_stream(tx, "TX")
        self.assertTrue(any("XfrBlock" in ln for ln in lines))
        self.assertTrue(any("S(ifs request)" in ln for ln in lines))
        self.assertTrue(any("OK" in ln for ln in lines))

    def test_decode_flags_bad_edc(self):
        blk = bytearray(gempc_t1.i_block(0, 0, b"\x6a\x82"))
        blk[-1] ^= 0xFF
        rx = gempc_t1.make_frame(0x80, 9, bytes(blk))
        lines = psd.decode_stream(rx, "RX")
        self.assertTrue(any("BAD" in ln for ln in lines))

    def test_main_end_to_end_with_fd_pin(self):
        ifs_xfr = gempc_t1.make_frame(0x6F, 9, gempc_t1.ifs_request())
        reply = gempc_t1.make_frame(0x80, 9, gempc_t1.t1_block(0xE1, b"\xfe"))
        text = (f"12345  write(6, {strace_quote(ifs_xfr)}, {len(ifs_xfr)})"
                f" = {len(ifs_xfr)}\n"
                f"12345  read(6, {strace_quote(ifs_xfr + reply)}, "
                f"{len(ifs_xfr) + len(reply)}) = {len(ifs_xfr) + len(reply)}"
                "\n")
        with tempfile.NamedTemporaryFile("w", suffix=".txt",
                                         delete=False) as fh:
            fh.write(text)
            path = fh.name
        try:
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                rc = psd.main([path, "--fd", "6"])
        finally:
            Path(path).unlink(missing_ok=True)
        self.assertEqual(rc, 0)
        text_out = out.getvalue()
        self.assertIn("pinned fd=6", text_out)
        self.assertIn("XfrBlock", text_out)
        self.assertIn("DataBlock", text_out)
        self.assertIn("S(ifs response)", text_out)
        self.assertNotIn("WARNING", text_out)  # clean -xx capture

    def test_main_warns_on_legacy_capture(self):
        # a capture made WITHOUT -xx carries letter escapes — the tool
        # must say so loudly instead of silently trusting the decode
        text = '12345  write(6, "\\f\\n\\t", 3) = 3\n'
        with tempfile.NamedTemporaryFile("w", suffix=".txt",
                                         delete=False) as fh:
            fh.write(text)
            path = fh.name
        try:
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                psd.main([path, "--fd", "6"])
        finally:
            Path(path).unlink(missing_ok=True)
        self.assertIn("WARNING", out.getvalue())
        self.assertIn("-xx", out.getvalue())


if __name__ == "__main__":
    unittest.main()
