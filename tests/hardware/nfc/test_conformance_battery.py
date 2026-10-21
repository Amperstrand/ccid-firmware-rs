"""Host unit tests for the conformance battery's quiet-window semantics.

#339 (Codex #145 P2, the GemPC twin of the fixed nucula quiet loop): a
dropped serial port can never satisfy the wire-silence check —
`quiet_bytes()` must raise when the reopen fails, and a successful
reopen must restart the FULL quiet window so post-reconnect bytes are
still observed. Fakes stand in for the port; no hardware required.

Run: python3 -m unittest tests/hardware/nfc/test_conformance_battery.py
(also collected by pytest).
"""

import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

# python-collect runs in a MINIMAL env (pytest only, no pyserial — the
# gate exists exactly to catch collection-time hardware imports). Stub
# the module when absent so importing the battery cannot fail there;
# the bench keeps the real pyserial. (#361 review P1)
try:
    import serial  # noqa: E402  (the battery imports it at module level)
except ModuleNotFoundError:
    import types as _types
    _serial_stub = _types.ModuleType("serial")

    class _StubSerialException(Exception):
        pass

    _serial_stub.SerialException = _StubSerialException
    sys.modules["serial"] = _serial_stub
    serial = _serial_stub

import conformance_battery  # noqa: E402


class _FakePort:
    """Minimal serial stand-in: only what quiet_bytes() touches."""

    timeout = 0.1

    def __init__(self):
        self.reads = 0


def _reader(port):
    """A Reader without hardware: __new__ + the fields quiet_bytes uses."""
    r = conformance_battery.Reader.__new__(conformance_battery.Reader)
    r.name = "fake-reader"
    r.port = "/dev/fake"
    r.seq = 0
    r.pending = {}
    r.s = port
    return r


class QuietBytesDropTests(unittest.TestCase):
    def test_dead_port_failed_reopen_raises_not_silence(self):
        """The #339 defect: read raising + reopen failing used to `break`
        and return b"" — PASSing the silence check on a dead reader. It
        must raise instead (a bench fault is not wire silence)."""
        class DeadPort(_FakePort):
            def read(self, n):
                raise serial.SerialException(
                    "device reports readiness to read but returned no data")

        r = _reader(DeadPort())
        r.reopen = lambda: False
        with self.assertRaises(AssertionError) as cm:
            r.quiet_bytes(0.2)
        self.assertIn("bench hardware fault", str(cm.exception))

    def test_successful_reopen_restarts_the_full_window(self):
        """Codex #145 P2 twin: the reconnect eats wall time (5 s of
        retries here); without the restart the leftover window (0 s) is
        observed and post-reconnect bytes are missed. With the restart,
        unsolicited bytes after the reopen still FAIL the silence check.
        """
        clock = {"t": 100.0}

        class DropThenAlive(_FakePort):
            def read(self, n):
                self.reads += 1
                if self.reads == 1:
                    raise serial.SerialException("CDC re-enumeration drop")
                clock["t"] += 0.05  # a working 0.1 s-timeout read cadence
                return b"\xde\xad"  # unsolicited bytes AFTER the reopen

        port = DropThenAlive()
        r = _reader(port)

        def reopen():
            clock["t"] += 5.0  # the documented close/open retry cycle
            return True

        r.reopen = reopen
        with mock.patch.object(conformance_battery.time, "time",
                               lambda: clock["t"]):
            got = r.quiet_bytes(1.0)
        # Without the fix the loop exits at the pre-reopen deadline
        # (clock 101.0 > deadline 101.0 after the 5 s reopen) and got is
        # b"" — the silence-PASS. With it, the full 1 s window runs
        # again and the unsolicited bytes are collected.
        self.assertTrue(got.startswith(b"\xde\xad"),
                        f"post-reopen bytes unobserved: {got!r}")
        self.assertGreaterEqual(port.reads, 20,
                                "the post-reopen window was not restarted")

    def test_quiet_wire_after_reopen_still_passes_and_terminates(self):
        """The restart must not turn into an infinite loop: a genuinely
        quiet post-reconnect wire returns b"" after the full window."""
        clock = {"t": 100.0}

        class DropThenQuiet(_FakePort):
            def read(self, n):
                self.reads += 1
                if self.reads == 1:
                    raise serial.SerialException("CDC re-enumeration drop")
                clock["t"] += 0.05
                return b""

        port = DropThenQuiet()
        r = _reader(port)

        def reopen():
            clock["t"] += 5.0
            return True

        r.reopen = reopen
        with mock.patch.object(conformance_battery.time, "time",
                               lambda: clock["t"]):
            got = r.quiet_bytes(1.0)
        self.assertEqual(got, b"")
        self.assertGreaterEqual(port.reads, 20)


if __name__ == "__main__":
    unittest.main()


class QuietBytesFlappingTests(unittest.TestCase):
    def test_flapping_port_is_bounded_not_infinite(self):
        """#361 review P2: a port that drops and reopens forever must not
        extend the quiet window without bound (the bench launchers'
        `timeout` kill bypasses cleanup while labgrid places are held
        and pcscd is stopped). After MAX_QUIET_REOPENS reopens the
        quiet check raises as a bench hardware fault."""
        clock = {"t": 100.0}

        class AlwaysDropping(_FakePort):
            def read(self, n):
                self.reads += 1
                raise serial.SerialException("flapping CDC")

        port = AlwaysDropping()
        r = _reader(port)
        r.reopen = lambda: (clock.__setitem__("t", clock["t"] + 0.2) or True)
        with mock.patch.object(conformance_battery.time, "time",
                               lambda: clock["t"]):
            with self.assertRaises(AssertionError) as cm:
                r.quiet_bytes(1.0)
        self.assertIn("flapping", str(cm.exception))
