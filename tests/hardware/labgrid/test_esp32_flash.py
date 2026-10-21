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

    def test_failed_pcscd_stop_aborts_and_releases_the_lock(self):
        # Codex P1 on #299 round 3: a discarded stop result leaves
        # pcscd.socket armed to re-grab the port mid-write — a failed
        # stop must raise (the flash path reports it in the verdict
        # without attempting any esptool op) and release the fd
        with tempfile.NamedTemporaryFile() as lockfile:
            orig_lock = esp32_flash.PCSCD_LOCK
            esp32_flash.PCSCD_LOCK = lockfile.name
            try:
                with self.assertRaises(RuntimeError):
                    esp32_flash.pcscd_lock_acquire(
                        run=lambda c, timeout=60: FakeResult(
                            1, err="Access denied"))
                # fd was released: a fresh non-blocking acquire works
                import fcntl
                probe = open(lockfile.name, "w")
                try:
                    fcntl.flock(probe,
                                fcntl.LOCK_EX | fcntl.LOCK_NB)
                finally:
                    probe.close()
            finally:
                esp32_flash.PCSCD_LOCK = orig_lock

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


class QuiesceUnitTests(unittest.TestCase):
    # #314: the nucula CDC is permanently held by the STANDING
    # nucula-ccid-broker service — fuser -k kills its process and
    # systemd restarts it onto the port MID-FLASH (the Checksum-error
    # class). The unit must be stopped under the SAME held pcscd flock
    # and restarted during restore, before pcscd.
    PORT = "/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_90:DA:72:9A:50:18-if00"

    def _flash(self, calls, unit_ok=True, unit="nucula-ccid-broker", **kw):
        import tempfile
        def run(cmd, timeout=300):
            calls.append(cmd)
            if "esptool" in cmd:
                return FakeResult(0)
            if unit and f"systemctl stop {unit}" in cmd and not unit_ok:
                return FakeResult(1, err="Unit not loaded")
            return FakeResult(0)
        with tempfile.NamedTemporaryFile() as lockfile:
            orig = esp32_flash.PCSCD_LOCK
            esp32_flash.PCSCD_LOCK = lockfile.name
            try:
                return esp32_flash.flash_esp32(
                    self.PORT, "esp32c3", {"0x40000": "/tmp/app.bin"},
                    run=run, usb_reset=lambda d: True, rebind=lambda p: True,
                    boot_check=lambda t: True,
                    capture=lambda p: "Calling app_main()",
                    quiesce_unit=unit, **kw)
            finally:
                esp32_flash.PCSCD_LOCK = orig

    def test_unit_stop_start_ordering_under_the_flock(self):
        calls = []
        ok, log = self._flash(calls)
        self.assertTrue(ok)
        stop_pcscd = next(i for i, c in enumerate(calls)
                          if "stop pcscd.socket pcscd" in c)
        stop_unit = next(i for i, c in enumerate(calls)
                         if "stop nucula-ccid-broker" in c)
        first_esptool = next(i for i, c in enumerate(calls)
                             if "esptool" in c)
        start_unit = next(i for i, c in enumerate(calls)
                          if "start nucula-ccid-broker" in c)
        start_pcscd = next(i for i, c in enumerate(calls)
                           if "start pcscd.socket pcscd" in c)
        # pcscd stop -> unit stop -> esptool(s) -> unit start -> pcscd start
        self.assertLess(stop_pcscd, stop_unit)
        self.assertLess(stop_unit, first_esptool)
        self.assertLess(first_esptool, start_unit)
        self.assertLess(start_unit, start_pcscd)
        self.assertEqual(
            [c for c in calls if "nucula-ccid-broker" in c].count(
                "systemctl stop nucula-ccid-broker"), 1)

    def test_unit_stop_failure_aborts_before_any_esptool(self):
        calls = []
        ok, log = self._flash(calls, unit_ok=False)
        self.assertFalse(ok)
        self.assertFalse(any("esptool" in c for c in calls))
        self.assertIn("QUIESCE FAILED", log)
        self.assertIn("nucula-ccid-broker", log)
        # restore still ran: pcscd was put back
        self.assertTrue(any("start pcscd.socket pcscd" in c for c in calls))

    def test_no_unit_flag_keeps_legacy_quiesce(self):
        # the flag is opt-in: a plain flash touches ONLY pcscd — no
        # other units are stopped/started
        calls = []
        ok, _ = self._flash(calls, unit=None)
        self.assertTrue(ok)
        unit_cmds = [c for c in calls if "systemctl" in c
                     and "pcscd" not in c]
        self.assertEqual(unit_cmds, [])


class WireBootCheckTests(unittest.TestCase):
    # #313: console-none families (xtensa .full, CONFIG_ESP_CONSOLE_NONE)
    # never print `Calling app_main()` — their operators were trained
    # into --no-boot-check + ad-hoc probe scripts. `boot_check="wire"`
    # makes the proof-of-boot a GetSlotStatus round-trip IN the flash
    # path.
    PORT = "/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-if00-port0"

    def _flash(self, wire):
        def run(cmd, timeout=300):
            return FakeResult(0)
        return esp32_flash.flash_esp32(
            self.PORT, "esp32", {"0x40000": "/tmp/app.bin"}, run=run,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: None, restore=lambda: None,
            boot_check="wire", wire_check=wire)

    def test_wire_pass_proves_boot(self):
        ok, log = self._flash(lambda p: (True, "GetSlotStatus: OK bmICCStatus=2"))
        self.assertTrue(ok)
        self.assertIn("wire boot check: passed", log)

    def test_wire_fail_fails_the_flash(self):
        ok, log = self._flash(
            lambda p: (False, "GetSlotStatus: NO LRC-valid reply"))
        self.assertFalse(ok)
        self.assertIn("BOOT CHECK FAILED (wire)", log)
        self.assertIn("wire boot check: FAILED", log)

    def test_wire_mode_does_not_touch_the_console_capture(self):
        # the console path (capture + signature check) must NOT run in
        # wire mode — console-none builds have no console to read
        used = []
        self._flash(lambda p: (True, "ok"))
        # (the injectable capture would record a call; wire mode passes
        # a capture that fails loudly if used)
        def run(cmd, timeout=300):
            return FakeResult(0)
        ok, _ = esp32_flash.flash_esp32(
            self.PORT, "esp32", {"0x40000": "/tmp/app.bin"}, run=run,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: None, restore=lambda: None,
            boot_check="wire",
            wire_check=lambda p: (True, "ok"),
            capture=lambda p: used.append(p) or (_ for _ in ()).throw(
                AssertionError("console capture used in wire mode")))
        self.assertTrue(ok)
        self.assertEqual(used, [])


class ObservabilityTests(unittest.TestCase):
    # 2026-10-20 c3 incident: the CLI printed its whole log only at
    # exit, so a multi-minute 115200 flash was indistinguishable from
    # a hang (the first attempt got killed at a tool cap mid-write).
    # The log now STREAMS with elapsed timestamps, esptool phases are
    # announced BEFORE running, and a SIGKILL'd flash leaves a
    # detectable pcscd-down marker.
    PORT = "/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-if00-port0"

    def test_streaming_log_prints_with_timestamp_when_echo(self):
        import io
        import contextlib
        buf = io.StringIO()
        log = esp32_flash.StreamingLog(echo=True)
        with contextlib.redirect_stdout(buf):
            log.append("phase one")
        out = buf.getvalue()
        self.assertIn("phase one", out)
        self.assertRegex(out, r"\[\s*\d+\.\ds\]")

    def test_streaming_log_silent_without_echo(self):
        import io
        import contextlib
        buf = io.StringIO()
        log = esp32_flash.StreamingLog(echo=False)
        with contextlib.redirect_stdout(buf):
            log.append("quiet")
        self.assertEqual(buf.getvalue(), "")
        self.assertIn("quiet", log)  # still a list

    def test_pcscd_marker_lifecycle_acquire_release(self):
        import tempfile
        from pathlib import Path as P
        with tempfile.NamedTemporaryFile() as lockfile:
            orig = esp32_flash.PCSCD_LOCK
            esp32_flash.PCSCD_LOCK = lockfile.name
            try:
                fd = esp32_flash.pcscd_lock_acquire(
                    run=lambda c, timeout=60: FakeResult(0))
                marker = P(esp32_flash.lock_marker_path(lockfile.name))
                self.assertTrue(marker.exists(),
                                "marker laid down while pcscd is down")
                esp32_flash.pcscd_lock_release(
                    fd, run=lambda c, timeout=60: FakeResult(0))
                self.assertFalse(marker.exists(),
                                 "marker removed after pcscd restart")
            finally:
                esp32_flash.PCSCD_LOCK = orig

    def test_stale_marker_recovery_restarts_pcscd(self):
        import tempfile
        from pathlib import Path as P
        cmds = []
        with tempfile.NamedTemporaryFile() as lockfile:
            orig = esp32_flash.PCSCD_LOCK
            esp32_flash.PCSCD_LOCK = lockfile.name
            try:
                P(esp32_flash.lock_marker_path(lockfile.name)).write_text(
                    "pcscd down since flash quiesce, pid=dead\n")
                recovered = esp32_flash.recover_stale_pcscd_down(
                    run=lambda c, timeout=60:
                    (cmds.append(c), FakeResult(0))[1])
                self.assertTrue(recovered)
                self.assertTrue(any("start pcscd" in c for c in cmds))
                self.assertFalse(
                    P(esp32_flash.lock_marker_path(lockfile.name)).exists())
            finally:
                esp32_flash.PCSCD_LOCK = orig

    def test_no_marker_no_recovery(self):
        import tempfile
        with tempfile.NamedTemporaryFile() as lockfile:
            orig = esp32_flash.PCSCD_LOCK
            esp32_flash.PCSCD_LOCK = lockfile.name
            try:
                self.assertFalse(esp32_flash.recover_stale_pcscd_down(
                    run=lambda c, timeout=60: FakeResult(0)))
            finally:
                esp32_flash.PCSCD_LOCK = orig

    def test_esptool_phase_announced_before_the_op(self):
        cmds = []
        seen = []

        class LogSpy(list):
            def __init__(self, echo=False):
                super().__init__()

            def append(self, item):
                seen.append(str(item))
                super().append(item)

        def run(cmd, timeout=300):
            cmds.append(cmd)
            return FakeResult(0)

        orig_log = esp32_flash.StreamingLog
        try:
            esp32_flash.StreamingLog = LogSpy
            ok, _ = esp32_flash.flash_esp32(
                self.PORT, "esp32", {"0x40000": "/tmp/app.bin"}, run=run,
                usb_reset=lambda d: True, rebind=lambda p: True,
                quiesce=lambda: None, restore=lambda: None,
                boot_check=lambda t: True,
                capture=lambda p: "Calling app_main()")
        finally:
            esp32_flash.StreamingLog = orig_log
        self.assertTrue(ok)
        # the "→ starting" line precedes the FIRST esptool invocation
        first_esptool = next(i for i, s in enumerate(seen)
                             if s.startswith("$ esptool"))
        self.assertTrue(any(s.startswith("→ esptool") for s in
                            seen[:first_esptool]),
                        f"no phase line before first esptool: {seen[:3]}")


class ConsoleNoneDefaultTests(unittest.TestCase):
    # #332 P2 on #324: console-none families (sdkconfig-xtensa.full,
    # sdkconfig.defaults.esp32 — CONFIG_ESP_CONSOLE_NONE=y) can never
    # print the console signature. With NO explicit boot_check the
    # proof-of-boot AUTO-defaults to the wire check; explicit flags
    # still win.
    PORT = ("/dev/serial/by-id/usb-Hades2001_M5stack_49D6163EBE-"
            "if00-port0")

    def _flash(self, console_none=True, boot_check=None, text=None,
               wire=(True, "GetSlotStatus: OK")):
        caps, wires = [], []

        def run(cmd, timeout=300):
            return FakeResult(0)

        ok, log = esp32_flash.flash_esp32(
            self.PORT, "esp32", {"0x40000": "/tmp/app.bin"}, run=run,
            usb_reset=lambda d: True, rebind=lambda p: True,
            quiesce=lambda: None, restore=lambda: None,
            boot_check=boot_check, console_none=console_none,
            capture=lambda p: caps.append(p)
            or (text if text is not None else "Calling app_main()"),
            wire_check=lambda p: wires.append(p) or wire)
        return ok, log, caps, wires

    def test_console_none_defaults_to_wire(self):
        ok, log, caps, wires = self._flash()
        self.assertTrue(ok)
        self.assertEqual(wires, [self.PORT])   # wire check ran
        self.assertEqual(caps, [])             # console never touched
        self.assertIn("auto-default", log)

    def test_console_none_explicit_optout_wins(self):
        ok, log, caps, wires = self._flash(boot_check=False)
        self.assertTrue(ok)
        self.assertIn("SKIPPED", log)
        self.assertEqual(wires, [])
        self.assertEqual(caps, [])

    def test_console_none_explicit_check_wins(self):
        ok, log, caps, wires = self._flash(
            boot_check=lambda t: "Calling app_main" in t)
        self.assertTrue(ok)
        self.assertEqual(caps, [self.PORT])    # console check ran
        self.assertEqual(wires, [])

    def test_console_build_keeps_the_console_default(self):
        # console_none=False (or omitted): nothing changes — the
        # default console check still governs, wire never runs
        ok, log, caps, wires = self._flash(
            console_none=False,
            text="E (250) i2c: CONFLICT! driver_ng is not allowed")
        self.assertFalse(ok)
        self.assertIn("BOOT CHECK FAILED", log)
        self.assertEqual(caps, [self.PORT])
        self.assertEqual(wires, [])


class CliBootCheckResolutionTests(unittest.TestCase):
    # #332: the CLI layer must forward the auto-default inputs so the
    # LIBRARY does the defaulting (one code path), while an explicit
    # --boot-check console must beat --console-none instead of being
    # silently flipped to the wire check.
    def _cli(self, *extra):
        import io
        import contextlib
        captured = {}

        def fake_flash(port, chip, images, **kw):
            captured.update(kw)
            return True, "stub"

        orig_flash = esp32_flash.flash_esp32
        orig_recover = esp32_flash.recover_stale_pcscd_down
        orig_argv = sys.argv
        esp32_flash.flash_esp32 = fake_flash
        esp32_flash.recover_stale_pcscd_down = lambda run=None: False
        sys.argv = ["esp32_flash.py", "--port", "/dev/null",
                    "--chip", "esp32", "--image", "0x40000=/tmp/app.bin",
                    *extra]
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                rc = esp32_flash._cli()
        finally:
            esp32_flash.flash_esp32 = orig_flash
            esp32_flash.recover_stale_pcscd_down = orig_recover
            sys.argv = orig_argv
        self.assertEqual(rc, 0)
        return captured

    def test_console_none_forwards_and_threads_wire_check(self):
        kw = self._cli("--console-none")
        self.assertTrue(kw["console_none"])
        self.assertIsNone(kw["boot_check"])   # library auto-defaults
        self.assertIsNotNone(kw["wire_check"])  # --stopbits threading

    def test_explicit_console_beats_console_none(self):
        kw = self._cli("--console-none", "--boot-check", "console")
        self.assertFalse(kw["console_none"])
        self.assertIsNone(kw["boot_check"])   # plain console default

    def test_explicit_no_boot_check_beats_console_none(self):
        kw = self._cli("--console-none", "--no-boot-check")
        self.assertFalse(kw["boot_check"])
        self.assertTrue(kw["console_none"])   # forwarded, but inert

    def test_plain_invocation_is_unchanged(self):
        kw = self._cli()
        self.assertFalse(kw["console_none"])
        self.assertIsNone(kw["boot_check"])


class UnitRestartRaiseTests(unittest.TestCase):
    # #332 P2 on #324: a RAISING unit restart (e.g. a subprocess
    # TimeoutExpired out of run()) used to propagate out of restore()
    # BEFORE pcscd_lock_release — stranding the held bench flock in a
    # long-lived caller. The raise is reported and the pcscd restore
    # (and the lock release) must still happen.
    PORT = ("/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_"
            "90:DA:72:9A:50:18-if00")

    def test_unit_restart_raise_still_releases_the_bench_lock(self):
        import fcntl
        import tempfile
        calls = []
        seen = []

        class LogSpy(list):
            def __init__(self, echo=False):
                super().__init__()

            def append(self, item):
                seen.append(str(item))
                super().append(item)

        def run(cmd, timeout=300):
            calls.append(cmd)
            if "systemctl start nucula-ccid-broker" in cmd:
                raise RuntimeError("systemd lost the bus")
            return FakeResult(0)

        orig_log = esp32_flash.StreamingLog
        with tempfile.NamedTemporaryFile() as lockfile:
            orig_lock = esp32_flash.PCSCD_LOCK
            esp32_flash.PCSCD_LOCK = lockfile.name
            esp32_flash.StreamingLog = LogSpy
            try:
                ok, _ = esp32_flash.flash_esp32(
                    self.PORT, "esp32c3", {"0x40000": "/tmp/app.bin"},
                    run=run, usb_reset=lambda d: True,
                    rebind=lambda p: True,
                    boot_check=lambda t: True,
                    capture=lambda p: "Calling app_main()",
                    quiesce_unit="nucula-ccid-broker")
                self.assertTrue(ok)
                self.assertTrue(any("RESTART RAISED" in s for s in seen),
                                f"no raise warning in log: {seen}")
                # pcscd restore ran DESPITE the raise
                self.assertTrue(any("start pcscd.socket pcscd" in c
                                    for c in calls))
                # the flock was NOT stranded: a fresh non-blocking
                # acquire on the same file must succeed
                probe = open(lockfile.name, "w")
                try:
                    fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                finally:
                    probe.close()
            finally:
                esp32_flash.PCSCD_LOCK = orig_lock
                esp32_flash.StreamingLog = orig_log


class MarkerUntilRestartTests(unittest.TestCase):
    # #332 P2 on #324: the .pcscd-down marker is cleared ONLY on a
    # VERIFIED pcscd restart — a failed/raising `systemctl start` used
    # to clear it in the finally, losing the only recoverable record
    # that pcscd (and every other bench reader) is down.

    def test_marker_kept_when_restart_fails(self):
        import fcntl
        import tempfile
        from pathlib import Path as P
        with tempfile.NamedTemporaryFile() as lockfile:
            orig = esp32_flash.PCSCD_LOCK
            esp32_flash.PCSCD_LOCK = lockfile.name
            try:
                fd = esp32_flash.pcscd_lock_acquire(
                    run=lambda c, timeout=60: FakeResult(0))
                marker = P(esp32_flash.lock_marker_path(lockfile.name))
                self.assertTrue(marker.exists())
                pcscd_ok, _ = esp32_flash.pcscd_lock_release(
                    fd, run=lambda c, timeout=60:
                    FakeResult(1, err="start request repeated too quickly"),
                    warn=lambda msg: None)
                self.assertFalse(pcscd_ok)
                self.assertTrue(marker.exists(),
                                "marker cleared on a DEAD pcscd — the "
                                "stale-marker recovery can never fire")
                # the lock itself was still released
                probe = open(lockfile.name, "w")
                try:
                    fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                finally:
                    probe.close()
                # and the stale-marker recovery can now put pcscd back
                self.assertTrue(esp32_flash.recover_stale_pcscd_down(
                    run=lambda c, timeout=60: FakeResult(0)))
                self.assertFalse(marker.exists())
            finally:
                esp32_flash.PCSCD_LOCK = orig

    def test_marker_kept_when_restart_raises(self):
        import fcntl
        import subprocess
        import tempfile
        from pathlib import Path as P
        with tempfile.NamedTemporaryFile() as lockfile:
            orig = esp32_flash.PCSCD_LOCK
            esp32_flash.PCSCD_LOCK = lockfile.name
            try:
                fd = esp32_flash.pcscd_lock_acquire(
                    run=lambda c, timeout=60: FakeResult(0))
                marker = P(esp32_flash.lock_marker_path(lockfile.name))

                def run(cmd, timeout=60):
                    raise subprocess.TimeoutExpired(cmd, timeout)

                pcscd_ok, _ = esp32_flash.pcscd_lock_release(
                    fd, run=run, warn=lambda msg: None)
                self.assertFalse(pcscd_ok)
                self.assertTrue(marker.exists())
                # lock released even though the restart raised
                probe = open(lockfile.name, "w")
                try:
                    fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
                finally:
                    probe.close()
            finally:
                esp32_flash.PCSCD_LOCK = orig

    def test_stale_marker_recovery_keeps_marker_on_failed_restart(self):
        import tempfile
        from pathlib import Path as P
        with tempfile.NamedTemporaryFile() as lockfile:
            orig = esp32_flash.PCSCD_LOCK
            esp32_flash.PCSCD_LOCK = lockfile.name
            try:
                marker = P(esp32_flash.lock_marker_path(lockfile.name))
                marker.write_text("pcscd down since flash quiesce\n")
                self.assertTrue(esp32_flash.recover_stale_pcscd_down(
                    run=lambda c, timeout=60: FakeResult(1)))
                self.assertTrue(
                    marker.exists(),
                    "recovery cleared the marker on a failed restart")
                # a later successful retry clears it
                self.assertTrue(esp32_flash.recover_stale_pcscd_down(
                    run=lambda c, timeout=60: FakeResult(0)))
                self.assertFalse(marker.exists())
            finally:
                esp32_flash.PCSCD_LOCK = orig


class EntryPointTests(unittest.TestCase):
    # #332 P2s on #324: the in-repo flash entry points must actually
    # USE the tool's protections — the nucula flow passes
    # --quiesce-unit nucula-ccid-broker (not opt-in-only), and
    # console-none builds default their boot proof to the wire check.
    REPO = Path(__file__).resolve().parents[3]
    BUILD_SH = REPO / "firmware" / "esp32-ccid" / "build.sh"
    FLASH_AND_TEST = REPO / "firmware" / "esp32-ccid" / "flash_and_test.sh"

    # build.sh resolves the partition table under CARGO_TARGET_DIR and
    # runs `set -euo pipefail` — a missing glob match would abort the
    # script before the flash line, so each dry run gets a fake target
    # dir carrying a partition-table.bin at the expected path.
    BOARDS = {
        "c3-ccid": ("riscv32imc-esp-espidf", "debug"),
        "m5stick": ("xtensa-esp32-espidf", "release"),
    }

    def _dry_run_flash_cmd(self, board, env=None):
        import os
        import subprocess
        import tempfile
        triple, profile = self.BOARDS[board]
        td = tempfile.TemporaryDirectory()
        self.addCleanup(td.cleanup)
        pt = (Path(td.name) / triple / profile / "build"
              / "esp-idf-sys-dryrun" / "out" / "build"
              / "partition_table" / "partition-table.bin")
        pt.parent.mkdir(parents=True)
        pt.write_bytes(b"\xeb\xeb\xa7\x00" + b"\0" * 28)
        r = subprocess.run(
            ["bash", str(self.BUILD_SH), board, "--flash",
             "/dev/serial/by-id/dry-run-port", "--dry-run"],
            capture_output=True, text=True, timeout=120,
            env={**os.environ, "CARGO_TARGET_DIR": td.name,
                 **(env or {})})
        self.assertEqual(r.returncode, 0, r.stderr)
        line = next(l for l in r.stdout.splitlines()
                    if "esp32_flash.py" in l)
        return line

    def test_nucula_flash_passes_the_broker_quiesce_unit(self):
        # the nucula CDC is held by the standing broker — the c3 board
        # family must quiesce it, no longer opt-in
        line = self._dry_run_flash_cmd("c3-ccid")
        self.assertIn("--quiesce-unit nucula-ccid-broker", line)

    def test_console_board_does_not_claim_console_none(self):
        line = self._dry_run_flash_cmd("c3-ccid")
        self.assertNotIn("--console-none", line)
        self.assertNotIn("--boot-check", line)

    def test_console_none_board_defaults_to_wire(self):
        # sdkconfig-xtensa.full is console-none: the boot proof rides
        # the wire check without the operator remembering the flag
        line = self._dry_run_flash_cmd("m5stick")
        self.assertIn("--console-none", line)
        self.assertNotIn("--quiesce-unit", line)  # xtensa ≠ nucula

    def test_explicit_boot_check_env_overrides_the_default(self):
        line = self._dry_run_flash_cmd(
            "m5stick", env={"ESP_BOOT_CHECK": "console"})
        self.assertIn("--boot-check console", line)
        self.assertNotIn("--console-none", line)

    def test_quiesce_unit_env_overrides_the_nucula_default(self):
        # escape hatch for hosts without the broker deployed: an EMPTY
        # ESP_QUIESCE_UNIT suppresses the flag entirely; a named unit
        # is passed through verbatim
        line = self._dry_run_flash_cmd("c3-ccid",
                                       env={"ESP_QUIESCE_UNIT": ""})
        self.assertNotIn("--quiesce-unit", line)
        line = self._dry_run_flash_cmd(
            "m5stick", env={"ESP_QUIESCE_UNIT": "other-owner.service"})
        self.assertIn("--quiesce-unit other-owner.service", line)

    def test_flash_and_test_defaults_to_wire(self):
        # its sdkconfig.defaults.esp32 flow is console-none — the
        # esp32_flash.py invocation must carry the wire default (no
        # dry-run mode exists for this script, so the invocation is
        # asserted on source)
        text = self.FLASH_AND_TEST.read_text()
        idx = text.rindex("esp32_flash.py")  # the invocation, not the header
        window = text[max(0, idx - 200):idx + 2000]
        self.assertIn("--console-none", window)
        self.assertIn("ESP_BOOT_CHECK", window)


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


class VerdictCarriesRestoreTest(unittest.TestCase):
    def test_restore_warning_rides_returned_log(self):
        # #352 (Codex P2): the DEFAULT restore() appends warnings to the
        # function's log AFTER the verdict returns in source order — the
        # deferred join must include them.
        import subprocess
        import esp32_flash as ef

        def failing_run(cmd, timeout):
            raise subprocess.TimeoutExpired(cmd, timeout)

        ok, log = ef.flash_esp32(
            "/dev/null", "esp32",
            {"0x40000": "/tmp/nonexistent.bin"},
            quiesce_unit="broken-unit",
            run=failing_run,
            boot_check=False, usb_reset=lambda dev: True,
            rebind=lambda port: True,
        )
        self.assertFalse(ok)
        self.assertIn("RESTART RAISED", log,
                      "restore warnings must ride the returned verdict")
