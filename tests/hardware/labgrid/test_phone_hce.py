"""Unit tests for phone_hce.py — the #529 place/remove card verbs.

Fully offline: adb is faked (command recording), pcscd presence is
faked via a stub smartcard module injected at call time (the lazy
import inside await_presence keeps collection safe on minimal hosts).

Run: python3 -m unittest tests/hardware/labgrid/test_phone_hce.py
"""

import sys
import time
import types
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))  # labgrid/, the module's dir

import phone_hce  # noqa: E402


class FakeAdb:
    """Records adb invocations; answers from a scripted reply table."""

    def __init__(self, replies=None):
        self.calls = []
        self.replies = replies or {}

    def __call__(self, serial, *args, timeout=30):
        self.calls.append((serial, tuple(args)))
        key = tuple(args)
        for prefix, reply in self.replies.items():
            if key[:len(prefix)] == prefix:
                if isinstance(reply, Exception):
                    raise reply
                return reply
        return ""


NFCDUMP = (
    "State: on\n"
    "Screen State: ON_UNLOCKED\n"
    "ComponentInfo{dev.boltcard.hce/dev.boltcard.hce.Svc} (UID: 1)\n"
    "    On Host Service\n"
)


class VerbTests(unittest.TestCase):
    def setUp(self):
        self.fake = FakeAdb({
            ("shell", "dumpsys", "nfc"): NFCDUMP,
            ("shell", "settings", "get", "secure",
             "nfc_payment_default_component"): "com.google.android.gms/x",
            ("shell", "pidof", "com.amperstrand.benchrelay"):
                phone_hce.AdbError("not found"),
        })
        self.patch = mock.patch.object(phone_hce, "adb", self.fake)
        self.patch.start()
        self.addCleanup(self.patch.stop)
        self.hce = phone_hce.PhoneHce("SERIAL")

    def test_remove_card_uses_the_definitive_verb(self):
        # #529: multi-HCE routing means pm disable does NOT remove the
        # card — removal must be svc nfc disable.
        self.hce.FIELD_CYCLE_SETTLE_S = 0
        self.hce.remove_card()
        self.assertIn(("SERIAL", ("shell", "svc", "nfc", "disable")),
                      self.fake.calls)
        self.assertFalse(any("pm disable" in " ".join(c[1]) and
                             "svc" not in " ".join(c[1])
                             for c in self.fake.calls))

    def test_place_card_plain_wakes_and_enables_nfc(self):
        self.hce.FIELD_CYCLE_SETTLE_S = 0
        self.hce.place_card("plain")
        joined = [" ".join(c[1]) for c in self.fake.calls]
        self.assertTrue(any("KEYCODE_WAKEUP" in j for j in joined))
        self.assertTrue(any("svc power stayon true" in j for j in joined))
        self.assertTrue(any("svc nfc enable" in j for j in joined))
        self.assertTrue(any("deviceidle whitelist +com.amperstrand.benchrelay"
                            in j for j in joined))

    def test_place_card_relay_takes_payment_default_and_forwards(self):
        # The Samsung dispatch workaround: payment-category routing +
        # payment default + the kickstart FGS + the adb forward.
        self.hce.FIELD_CYCLE_SETTLE_S = 0
        self.hce.place_card("relay")
        joined = [" ".join(c[1]) for c in self.fake.calls]
        self.assertTrue(any("start-foreground-service" in j and
                            "RelayKickstartService" in j for j in joined))
        self.assertTrue(any("forward" in j and "localabstract:bench-hce-relay"
                            in j for j in joined))
        self.assertTrue(any("nfc_payment_default_component" in j and
                            "benchrelay" in j and "put" in j
                            for j in joined))

    def test_status_parses_nfc_state_and_screen(self):
        st = self.hce.status()
        self.assertEqual(st["nfc_state"], "on")
        self.assertTrue(st["screen_on"])
        self.assertFalse(st["relay_process"])
        self.assertIn("dev.boltcard.hce/dev.boltcard.hce.Svc",
                      st["hce_services"])


class AwaitPresenceTests(unittest.TestCase):
    def _stub_smartcard(self, present_sequence):
        """Inject a fake smartcard module; yield the readers call count."""
        states = iter(present_sequence)
        mod = types.ModuleType("smartcard")
        sys_mod = types.ModuleType("smartcard.System")

        class FakeConn:
            def connect(self):
                if not next(states):
                    raise RuntimeError("no card")

        class FakeReader:
            def __str__(self):
                return "M5stick GemPC fake 00 00"

            def createConnection(self):
                return FakeConn()

        sys_mod.readers = lambda: [FakeReader()]
        mod.System = sys_mod
        orig = sys.modules.get("smartcard")
        sys.modules["smartcard"] = mod
        sys.modules["smartcard.System"] = sys_mod

        def restore():
            if orig is None:
                sys.modules.pop("smartcard", None)
                sys.modules.pop("smartcard.System", None)
            else:
                sys.modules["smartcard"] = orig
        return restore

    def test_presence_confirmed(self):
        restore = self._stub_smartcard([True])
        self.addCleanup(restore)
        hce = phone_hce.PhoneHce(None)
        self.assertTrue(hce.await_presence("M5stick", want=True,
                                           timeout=5))

    def test_absence_confirmed(self):
        restore = self._stub_smartcard([False])
        self.addCleanup(restore)
        hce = phone_hce.PhoneHce(None)
        self.assertTrue(hce.await_presence("M5stick", want=False,
                                           timeout=5))

    def test_wrong_state_times_out_fast(self):
        restore = self._stub_smartcard(itertools_cycle([False]))
        self.addCleanup(restore)
        hce = phone_hce.PhoneHce(None)
        t0 = time.time()
        self.assertFalse(hce.await_presence("M5stick", want=True,
                                            timeout=0.2))
        self.assertLess(time.time() - t0, 5)


def itertools_cycle(seq):
    while True:
        yield from seq


if __name__ == "__main__":
    unittest.main()
