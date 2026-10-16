"""Host unit tests for the canonical F469 flash path (no hardware).

Run: python3 -m unittest tests/hardware/labgrid/test_stm32_flash.py
(also collected by pytest).
"""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import stm32_flash  # noqa: E402


class FakeResult:
    def __init__(self, rc=0, out="", err=""):
        self.returncode = rc
        self.stdout = out
        self.stderr = err


class FlashArgTests(unittest.TestCase):
    def test_write_uses_connect_under_reset_and_pinned_serial(self):
        # bolty/micronuts lessons locked: --connect-under-reset on BOTH
        # the write and the reset, serial always pinned.
        cmds = []

        def run(cmd, timeout=60):
            cmds.append(cmd)
            return FakeResult(0)

        ok, _ = stm32_flash.flash_f469("/tmp/fw.bin", "ABC123", run=run)
        self.assertTrue(ok)
        self.assertEqual(cmds[0],
                         "st-flash --connect-under-reset --serial ABC123 "
                         "write /tmp/fw.bin 0x08000000")
        self.assertEqual(cmds[1],
                         "st-flash --connect-under-reset --serial ABC123 reset")

    def test_write_failure_triggers_usb_reset_and_one_retry(self):
        cmds = []
        resets = []

        def run(cmd, timeout=60):
            cmds.append(cmd)
            # first write fails (latch), retry succeeds
            return FakeResult(1 if len(cmds) == 1 else 0, err="SwdDpWait")

        def reset(dev):
            resets.append(dev)
            return True

        def lsusb():
            return ("Bus 001 Device 104: ID 0483:374b STMicroelectronics "
                    "ST-LINK/V2.1 066FFFABC123")

        orig = stm32_flash.stlink_usb_path
        stm32_flash.stlink_usb_path = lambda s, o=None: (
            "/dev/bus/usb/001/104" if lsusb() else None)
        try:
            ok, log = stm32_flash.flash_f469(
                "/tmp/fw.bin", "ABC123", run=run, reset=reset)
        finally:
            stm32_flash.stlink_usb_path = orig
        self.assertTrue(ok)
        self.assertEqual(resets, ["/dev/bus/usb/001/104"])
        self.assertEqual(cmds[0], cmds[1])  # identical retry command
        self.assertIn("USBDEVFS_RESET", log)

    def test_persistent_failure_reports_not_ok(self):
        def run(cmd, timeout=60):
            return FakeResult(1, err="SwdDpWait")

        ok, log = stm32_flash.flash_f469(
            "/tmp/fw.bin", "ABC123", run=run,
            reset=lambda _d: True)
        # stlink_usb_path returns None locally (no lsusb match on a dev
        # box) -> no reset attempted, straight failure
        self.assertFalse(ok)

    def test_failed_post_write_reset_fails_the_flash(self):
        n = {"i": 0}

        def run(cmd, timeout=60):
            n["i"] += 1
            # write ok, reset fails
            return FakeResult(0 if n["i"] == 1 else 1)

        ok, _ = stm32_flash.flash_f469("/tmp/fw.bin", "ABC123", run=run)
        self.assertFalse(ok)


class ParserTests(unittest.TestCase):
    def _sysfs_tree(self, base, serial, busnum, devnum):
        d = base / "1-1"
        d.mkdir(parents=True)
        (d / "serial").write_text(serial)
        (d / "busnum").write_text(str(busnum))
        (d / "devnum").write_text(str(devnum))
        return base

    def test_stlink_usb_path_resolves_serial_via_sysfs(self, tmp=None):
        # sysfs shape, NOT lsusb text (plain lsusb carries no serials —
        # the Codex P1 on #216: an lsusb-text matcher is dead code).
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            from pathlib import Path as P
            base = self._sysfs_tree(P(td), "066FFF515786534867184152", 1, 104)
            self.assertEqual(
                stm32_flash.stlink_usb_path("066FFF515786534867184152",
                                            sysfs_root=str(base)),
                "/dev/bus/usb/001/104")

    def test_stlink_usb_path_no_match_returns_none(self):
        with tempfile.TemporaryDirectory() as td:
            from pathlib import Path as P
            base = self._sysfs_tree(P(td), "066EFF3837334D4E", 1, 105)
            self.assertIsNone(
                stm32_flash.stlink_usb_path("066FFF5157", sysfs_root=str(base)))

    def test_stlink_usb_path_missing_sysfs_returns_none(self):
        self.assertIsNone(
            stm32_flash.stlink_usb_path("066FFF", sysfs_root="/nonexistent"))


if __name__ == "__main__":
    unittest.main()
