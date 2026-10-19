"""Host unit tests for the canonical ESP32 flash path (no hardware).

Run: python3 -m unittest tests/hardware/labgrid/test_esp32_flash.py
(also collected by pytest).
"""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import esp32_flash  # noqa: E402


class FakeResult:
    def __init__(self, rc=0, out="", err=""):
        self.returncode = rc
        self.stdout = out
        self.stderr = err


class SerialParseTests(unittest.TestCase):
    def test_hades_serial(self):
        self.assertEqual(
            esp32_flash.serial_of_port(
                "/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-if00-port0"),
            "49D6163EBE")

    def test_espressif_serial(self):
        self.assertEqual(
            esp32_flash.serial_of_port(
                "/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_"
                "90:DA:72:9A:50:18-if00"),
            "90:DA:72:9A:50:18")

    def test_non_byid_returns_none(self):
        self.assertIsNone(esp32_flash.serial_of_port("/dev/ttyUSB0"))


class BaudDisciplineTests(unittest.TestCase):
    def _exporter(self, speed=115200):
        td = tempfile.TemporaryDirectory()
        self.addCleanup(td.cleanup)
        p = Path(td.name) / "exporter.yaml"
        p.write_text(
            "m5stick-usb:\n  SerialPort:\n"
            "    port: /dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE"
            "-if00-port0\n"
            f"    speed: {speed}\n")
        return p

    PORT = ("/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-"
            "if00-port0")

    def test_caller_baud_above_config_is_downgraded(self):
        # the 2026-10-19 lesson: 460800 against a configured 115200
        cmds = []

        def run(cmd, timeout=300):
            cmds.append(cmd)
            return FakeResult(0)

        ok, log = esp32_flash.flash_esp32(
            self.PORT, "esp32", {"0x40000": "/tmp/app.bin"}, baud=460800,
            exporter_cfg=self._exporter(), run=run,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: None, restore=lambda: None)
        self.assertTrue(ok)
        self.assertIn("--baud 115200", cmds[0])
        self.assertIn("downgraded", log)

    def test_no_caller_baud_uses_configured(self):
        cmds = []

        def run(cmd, timeout=300):
            cmds.append(cmd)
            return FakeResult(0)

        ok, _ = esp32_flash.flash_esp32(
            self.PORT, "esp32", {"0x40000": "/tmp/app.bin"},
            exporter_cfg=self._exporter(), run=run,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: None, restore=lambda: None)
        self.assertTrue(ok)
        self.assertIn("--baud 115200", cmds[0])


class OrderingTests(unittest.TestCase):
    PORT = "/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-if00-port0"

    def test_writes_before_erases_single_invocation(self):
        # 2026-10-19 lesson lock: erasing otadata BEFORE writing is how
        # a board gets stranded; writes also land in ONE esptool call.
        cmds = []

        def run(cmd, timeout=300):
            cmds.append(cmd)
            return FakeResult(0)

        ok, _ = esp32_flash.flash_esp32(
            self.PORT, "esp32",
            {"0x8000": "/tmp/pt.bin", "0x40000": "/tmp/app.bin"},
            erase_regions=((0x30000, 0x2000),), run=run,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: None, restore=lambda: None)
        self.assertTrue(ok)
        self.assertEqual(len(cmds), 3)  # write, erase, final reset
        self.assertIn("write-flash", cmds[0])
        self.assertIn("0x8000 /tmp/pt.bin 0x40000 /tmp/app.bin", cmds[0])
        self.assertIn("erase-region 0x30000 0x2000", cmds[1])
        self.assertIn("hard-reset flash-id", cmds[2])

    def test_failed_write_never_erases(self):
        cmds = []

        def run(cmd, timeout=300):
            cmds.append(cmd)
            return FakeResult(1, err="Serial data stream stopped")

        ok, log = esp32_flash.flash_esp32(
            self.PORT, "esp32", {"0x40000": "/tmp/app.bin"},
            erase_regions=((0x30000, 0x2000),), run=run,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: None, restore=lambda: None)
        self.assertFalse(ok)
        self.assertFalse(any("erase-region" in c for c in cmds))
        self.assertIn("physical-replug", log)
        self.assertIn("previous image still boots", log)


class LadderTests(unittest.TestCase):
    PORT = "/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-if00-port0"

    def test_usb_reset_then_retry_recovers(self):
        cmds = []
        resets = []

        def run(cmd, timeout=300):
            cmds.append(cmd)
            return FakeResult(1 if len(cmds) == 1 else 0)

        ok, log = esp32_flash.flash_esp32(
            self.PORT, "esp32", {"0x40000": "/tmp/app.bin"}, run=run,
            usb_reset=lambda d: resets.append(d) or True,
            rebind=lambda p: True,
            usb_path=lambda s: "/dev/bus/usb/001/104",
            quiesce=lambda: None, restore=lambda: None)
        self.assertTrue(ok)
        self.assertEqual(resets, ["/dev/bus/usb/001/104"])
        self.assertIn("USBDEVFS_RESET", log)

    def test_persistent_failure_reports_physical_replug(self):
        def run(cmd, timeout=300):
            return FakeResult(1, err="readiness to read but returned no data")

        ok, log = esp32_flash.flash_esp32(
            self.PORT, "esp32", {"0x40000": "/tmp/app.bin"}, run=run,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: None, restore=lambda: None)
        self.assertFalse(ok)
        self.assertIn("physical-replug", log)


class HygieneTests(unittest.TestCase):
    PORT = "/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-if00-port0"

    def test_pcscd_quiesce_and_restore_always(self):
        # socket-activation lesson: BOTH units stopped before the first
        # esptool, restarted even when the flash fails.
        calls = []

        def run(cmd, timeout=300):
            calls.append(cmd)
            return FakeResult(1)

        esp32_flash.flash_esp32(
            self.PORT, "esp32", {"0x40000": "/tmp/app.bin"}, run=run,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: calls.append("QUIESCE"),
            restore=lambda: calls.append("RESTORE"))
        self.assertEqual(calls[0], "QUIESCE")
        self.assertEqual(calls[-1], "RESTORE")
        self.assertIn("RESTORE", calls)

    def test_failed_erase_reports_old_image_boot_risk(self):
        cmds = []

        def run(cmd, timeout=300):
            cmds.append(cmd)
            # write ok, erase fails through the whole ladder
            return FakeResult(0 if cmds[0] is cmd else 1)

        # simpler determinism: fail everything except the first call
        def run2(cmd, timeout=300):
            cmds.append(cmd)
            return FakeResult(0 if len(cmds) == 1 else 1)

        ok, log = esp32_flash.flash_esp32(
            self.PORT, "esp32", {"0x40000": "/tmp/app.bin"},
            erase_regions=((0x30000, 0x2000),), run=run2,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: None, restore=lambda: None)
        self.assertFalse(ok)
        self.assertIn("ERASE FAILED", log)
        self.assertIn("OLD image", log)


class BootCheckTests(unittest.TestCase):
    def test_conflict_aborts_fail_the_check(self):
        self.assertFalse(esp32_flash.boot_check_no_conflict(
            "E (250) i2c: CONFLICT! driver_ng is not allowed"))

    def test_clean_app_main_passes(self):
        self.assertTrue(esp32_flash.boot_check_no_conflict(
            "I (252) main_task: Calling app_main()\n(silence)"))

    def test_silent_console_passes(self):
        # xtensa classic-esp32 mains are console-silent post-claim; an
        # empty capture cannot fail the conflict gate
        self.assertTrue(esp32_flash.boot_check_no_conflict(""))


class SysfsTests(unittest.TestCase):
    def test_usb_dev_path_resolves_serial(self):
        with tempfile.TemporaryDirectory() as td:
            d = Path(td) / "1-8"
            d.mkdir()
            (d / "serial").write_text("49D6163EBE")
            (d / "busnum").write_text("1")
            (d / "devnum").write_text("81")
            self.assertEqual(
                esp32_flash.usb_dev_path_by_serial(
                    "49d6163ebe", sysfs_root=td),
                "/dev/bus/usb/001/081")

    def test_no_match_returns_none(self):
        self.assertIsNone(
            esp32_flash.usb_dev_path_by_serial("nope",
                                               sysfs_root="/nonexistent"))


if __name__ == "__main__":
    unittest.main()
