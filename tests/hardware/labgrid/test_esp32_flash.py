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
            quiesce=lambda: None, restore=lambda: None,
            boot_check=lambda t: True,
            capture=lambda p: "I (252) main_task: Calling app_main()")
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
            quiesce=lambda: None, restore=lambda: None,
            boot_check=lambda t: True,
            capture=lambda p: "I (252) main_task: Calling app_main()")
        self.assertTrue(ok)
        self.assertIn("--baud 115200", cmds[0])


class OrderingTests(unittest.TestCase):
    PORT = "/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-if00-port0"

    def test_writes_descending_before_erases_single_invocation(self):
        # 2026-10-19 lessons locked: erasing otadata BEFORE writing is
        # how a board gets stranded; writes land in ONE esptool call in
        # DESCENDING offset order — the app before the partition table,
        # so a mid-write failure leaves the OLD table standing.
        cmds = []

        def run(cmd, timeout=300):
            cmds.append(cmd)
            return FakeResult(0)

        ok, _ = esp32_flash.flash_esp32(
            self.PORT, "esp32",
            {"0x8000": "/tmp/pt.bin", "0x40000": "/tmp/app.bin"},
            erase_regions=((0x30000, 0x2000),), run=run,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: None, restore=lambda: None,
            boot_check=lambda t: True,
            capture=lambda p: "I (252) main_task: Calling app_main()")
        self.assertTrue(ok)
        self.assertEqual(len(cmds), 3)  # write, erase, final reset
        self.assertIn("write-flash", cmds[0])
        self.assertIn("0x40000 /tmp/app.bin 0x8000 /tmp/pt.bin", cmds[0])
        self.assertIn("erase-region 0x30000 0x2000", cmds[1])
        self.assertIn("hard-reset flash-id", cmds[2])

    def test_failed_write_never_erases_and_admits_partial_risk(self):
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
        self.assertIn("PARTIALLY", log)


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
            quiesce=lambda: None, restore=lambda: None,
            boot_check=lambda t: True,
            capture=lambda p: "I (252) main_task: Calling app_main()")
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

    def test_pcscd_flock_is_HELD_across_the_flash_window(self):
        # Codex P1 on #299: a per-command flock lets another bench
        # process lawfully restart pcscd mid-flash. The lock is an fd
        # held from quiesce until after restore restarted pcscd.
        import fcntl
        import tempfile
        calls = []

        def run(cmd, timeout=300):
            calls.append(cmd)
            return FakeResult(0)

        with tempfile.NamedTemporaryFile() as lockfile:
            orig_lock = esp32_flash.PCSCD_LOCK
            esp32_flash.PCSCD_LOCK = lockfile.name
            try:
                ok, _ = esp32_flash.flash_esp32(
                    self.PORT, "esp32", {"0x40000": "/tmp/app.bin"},
                    run=run, usb_reset=lambda d: True,
                    rebind=lambda p: True,
                    boot_check=lambda t: True,
                    capture=lambda p: "I (252) main_task: Calling "
                                      "app_main()")
            finally:
                esp32_flash.PCSCD_LOCK = orig_lock
        self.assertTrue(ok)
        # stop happens at acquire-time, start at release-time
        self.assertIn("stop pcscd.socket pcscd", calls[0])
        self.assertIn("start pcscd.socket pcscd", calls[-1])
        # the lock is actually RELEASED after the run: a non-blocking
        # acquire on the same file must succeed now
        fd = open(lockfile.name, "w")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        finally:
            fd.close()

    def test_failed_erase_reports_old_image_boot_risk(self):
        cmds = []

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
    PORT = "/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-if00-port0"

    def test_conflict_aborts_fail_the_check(self):
        self.assertFalse(esp32_flash.boot_check_no_conflict(
            "E (250) i2c: CONFLICT! driver_ng is not allowed"))

    def test_clean_app_main_passes(self):
        self.assertTrue(esp32_flash.boot_check_no_conflict(
            "I (252) main_task: Calling app_main()\n(silence)"))

    def test_rom_banner_only_FAILS(self):
        # Codex P1 on #299 round 2: ROM/bootloader banners replay on a
        # boot-looping image — firmware-level evidence (app_main) is
        # required
        self.assertFalse(esp32_flash.boot_check_no_conflict(
            "ESP-ROM:esp32-api1-20210207\nrst:0x1 (POWERON_RESET)\n"
            "load:0x3fcd5820\nE (250) i2c: ..."))

    def test_app_main_banner_on_rom_background_passes(self):
        self.assertTrue(esp32_flash.boot_check_no_conflict(
            "ESP-ROM:esp32-api1-20210207\nrst:0x1 (POWERON_RESET)\n"
            "I (252) main_task: Calling app_main()"))

    def test_pcscd_commands_are_NOT_shell_flock_wrapped(self):
        # the held fd IS the lock; a `flock <path>` wrapper opens a
        # second file description and self-deadlocks (empirically
        # confirmed: flock(1) blocks on a python-held LOCK_EX)
        calls = []

        def run(cmd, timeout=300):
            calls.append(cmd)
            return FakeResult(0)

        with tempfile.NamedTemporaryFile() as lockfile:
            orig_lock = esp32_flash.PCSCD_LOCK
            esp32_flash.PCSCD_LOCK = lockfile.name
            try:
                esp32_flash.flash_esp32(
                    self.PORT, "esp32", {"0x40000": "/tmp/app.bin"},
                    run=run, usb_reset=lambda d: True,
                    rebind=lambda p: True,
                    boot_check=lambda t: True,
                    capture=lambda p: "Calling app_main()")
            finally:
                esp32_flash.PCSCD_LOCK = orig_lock
        pcscd_cmds = [c for c in calls if "pcscd" in c]
        self.assertTrue(pcscd_cmds)
        for c in pcscd_cmds:
            self.assertFalse(c.strip().startswith("flock"), c)

    def test_empty_capture_FAILS(self):
        # Codex P1 on #299: no evidence is not proof — a dropped reset
        # or wedged bridge must not pass silently
        self.assertFalse(esp32_flash.boot_check_no_conflict(""))

    def test_boot_check_is_the_default_not_an_option(self):
        # Codex P1 on #299: with no boot_check passed, the flow uses
        # boot_check_no_conflict against the (injectable) capture — a
        # CONFLICT-y console fails an otherwise-successful flash.
        def run(cmd, timeout=300):
            return FakeResult(0)

        ok, log = esp32_flash.flash_esp32(
            self.PORT, "esp32", {"0x40000": "/tmp/app.bin"}, run=run,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: None, restore=lambda: None,
            capture=lambda p: "E (250) i2c: CONFLICT! driver_ng is not "
                              "allowed to be used with this old driver")
        self.assertFalse(ok)
        self.assertIn("BOOT CHECK FAILED", log)

    def test_explicit_optout_is_logged_loudly(self):
        def run(cmd, timeout=300):
            return FakeResult(0)

        ok, log = esp32_flash.flash_esp32(
            self.PORT, "esp32", {"0x40000": "/tmp/app.bin"}, run=run,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: None, restore=lambda: None,
            boot_check=False)
        self.assertTrue(ok)
        self.assertIn("SKIPPED", log)
        self.assertIn("NOT boot-proven", log)


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
