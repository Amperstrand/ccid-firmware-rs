"""Nucula board metadata and flash procedures.

Everything the test framework needs to know about the physical board:
pin assignments, flash layout, build variants, expected boot sequences,
and recovery procedures. Single source of truth — no scattered commands.

PORT LIFECYCLE (the hard-learned lessons):

The ESP32-C3's USB-Serial/JTAG is a composite device (CDC + JTAG).
When firmware with console output is running, the CDC endpoint is
actively streaming. Three failure modes:

1. PORT CONTENTION: logger/test holds port → esptool can't open it
   Fix: _free_port() uses fuser + pkill + explicit wait

2. USB-CDC RE-ENUMERATION: after flash, USB disconnects/reconnects
   Fix: ConsoleCapture polls the by-id path (follows the device)

3. USB PERIPHERAL STATE CORRUPTION: after certain flash sequences,
   the C3's USB peripheral stops responding to software reset
   Fix: board needs hard power cycle (replug or uhubctl port)

RULE: never run esptool while any process holds the serial port.
RULE: always flash bootloader + partition table + app together.
RULE: after full_erase, the board needs a USB replug.
"""

import hashlib
import os
import re
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

OPENOCD = "/opt/espressif/openocd-esp32/bin/openocd"


@dataclass
class NuculaBoard:
    """Physical board metadata and procedures."""

    # USB identification
    port_id: str = (
        "usb-Espressif_USB_JTAG_serial_debug_unit_90:DA:72:9A:50:18-if00"
    )
    port_path: str = f"/dev/serial/by-id/{port_id}"
    baud_flash: int = 460800
    baud_console: int = 115200

    # Flash layout (our partition table: ota_0 scheme)
    flash_offsets = {
        "bootloader": 0x0,
        "partition_table": 0x8000,
        "otadata": 0x30000,
        "ota_0_app": 0x40000,
        "ota_1_app": 0x1E0000,
        "coredump": 0x3F0000,
    }

    # I2C bus devices (verified via wallet firmware i2cscan)
    i2c_devices = {
        0x20: "PCF8574T keyboard",
        0x28: "PN7160 NFC controller",
        0x3C: "SSD1309 OLED (disconnected on this board)",
        0x7C: "unknown (fuel gauge/PMIC?)",
    }

    # I2C bus pins
    i2c_sda: int = 4
    i2c_scl: int = 5
    nfc_irq: int = 6
    nfc_ven: int = 7
    oled_power: int = 3  # GPIO3, LOW = boost off
    oled_reset: int = 10  # GPIO10, HIGH = reset asserted

    # Build paths (resolved at test time)
    workspace_root: Path = field(default_factory=lambda: Path(__file__).parent.parent.parent.parent)
    firmware_dir: Path = field(default_factory=lambda: Path(""))
    esp_idf_sys_out: Path = field(default_factory=lambda: Path(""))
    wallet_fw_dir: Path = Path("/tmp/opencode/nucula-fw/build-551")

    # Unknown-firmware gate (Codex PR-86 P1): how much of the ota_0
    # slot pretest_check reads to identify the running image, and
    # where remembered hashes live. The wallet app lands at 0x30000,
    # so the bytes at 0x40000 are wallet image offset 0x10000.
    app_peek_size: int = 0x1000
    wallet_app_base: int = 0x30000
    known_app_hashes_path: Path = Path("/tmp/opencode/nucula-app-region-hashes.txt")
    flash_backup_dir: Path = Path("/tmp/opencode")

    def __post_init__(self):
        self.firmware_dir = self.workspace_root / "firmware" / "esp32-ccid"
        self._resolve_esp_idf_sys_out()

    def _resolve_esp_idf_sys_out(self) -> None:
        """(Re)resolve the esp-idf-sys out dir with a complete IDF build.

        Lazy on purpose: a session that sweeps a contaminated cmake cache
        (the documented shared-target-dir fix) wipes the partition table
        AFTER this board object was constructed — resolving once in
        __post_init__ left esp_idf_sys_out as Path("") for the rest of
        the session. Multiple esp-idf-sys hash dirs can exist
        (feature/config drift); only ones with the partition table count.
        """
        target = Path("/root/.cargo-target/riscv32imc-esp-espidf")
        # CI guard: the path doesn't exist on hosted runners — and stat()
        # on a root-owned parent can EACCES there. Never raise at
        # collection time; resolution simply skips.
        try:
            if not target.exists():
                return
            candidates = [
                p for p in target.glob("debug/build/esp-idf-sys/*/out")
                if (p / "build" / "partition_table" / "partition-table.bin").exists()
            ]
        except OSError:
            return
        if candidates:
            self.esp_idf_sys_out = max(candidates, key=lambda p: p.stat().st_mtime)

    def _esp_idf_sys_out(self) -> Path:
        if not (self.esp_idf_sys_out / "build" / "partition_table" / "partition-table.bin").exists():
            self._resolve_esp_idf_sys_out()
        return self.esp_idf_sys_out

    @property
    def port(self) -> str:
        return self.port_path

    def _free_port(self):
        """Aggressively free the serial port from any process holding it."""
        self._suspend_consoles()
        tty = os.path.realpath(self.port_path)
        try:
            subprocess.run(["fuser", "-k", tty], capture_output=True, timeout=5)
        except (subprocess.TimeoutExpired, FileNotFoundError):
            pass

        subprocess.run(["pkill", "-9", "-f", "nucula_logger.py"], capture_output=True)
        subprocess.run(
            ["pkill", "-9", "-f", f"python.*{self.port_id}"],
            capture_output=True,
        )
        # Wait for the OS to release the port — do NOT open it here,
        # as that would create the very contention we're trying to prevent
        time.sleep(2)

    def _port_exists(self) -> bool:
        """Check if the USB port exists without opening it."""
        return os.path.exists(self.port_path)

    @staticmethod
    def _suspend_consoles():
        """Close live ConsoleCapture fds before `fuser -k` (Codex PR-86 P2).

        The session-scoped console fixture holds the port across tests;
        when this process is the holder, fuser -k kills pytest itself
        (bench-verified: SIGKILL, rc 137). Captures reconnect lazily on
        their next use, so suspending is transparent to tests."""
        try:
            from .console import suspend_all_captures
        except ImportError:
            try:
                from console import suspend_all_captures  # type: ignore
            except ImportError:
                return
        suspend_all_captures()

    def run_esptool(self, *args: str, timeout: int = 240,
                    retries: int = 3) -> subprocess.CompletedProcess:
        """Run esptool with port cleanup and retry logic.

        Retries handle the case where the port isn't fully released
        after the first attempt.
        """
        for attempt in range(retries):
            self._free_port()
            if not self._port_exists():
                raise FileNotFoundError(
                    f"USB port not found: {self.port_path}. "
                    "Is the board plugged in?"
                )
            cmd = [
                "esptool", "--chip", "esp32c3",
                "-p", self.port,
                "--baud", str(self.baud_flash),
                *args,
            ]
            try:
                result = subprocess.run(
                    cmd, capture_output=True, text=True, timeout=timeout,
                )
                if result.returncode == 0:
                    return result
                # Check for port-busy errors
                stderr = result.stderr or ""
                if "device reports readiness" in stderr or "Resource temporarily unavailable" in stderr:
                    continue  # retry
                return result  # non-port error, return as-is
            except subprocess.TimeoutExpired:
                if attempt < retries - 1:
                    continue
                raise

        return result  # last attempt's result

    def flash_app(self, binary: Path) -> bool:
        """Flash partition table + app atomically. Returns True on success."""
        pt = self._esp_idf_sys_out() / "build" / "partition_table" / "partition-table.bin"
        if not pt.exists():
            raise FileNotFoundError(f"Partition table not found: {pt}")
        if not binary.exists():
            raise FileNotFoundError(f"App binary not found: {binary}")
        result = self.run_esptool(
            "--after", "hard-reset", "write-flash",
            str(self.flash_offsets["partition_table"]), str(pt),
            str(self.flash_offsets["ota_0_app"]), str(binary),
        )
        return "Hash of data verified" in (result.stdout or "")

    def flash_full(self, bootloader: Path, partition_table: Path, app: Path) -> bool:
        """Flash bootloader + partition table + app in one operation.

        Always use this when changing from wallet ↔ our partition scheme.
        Flashing only the app leaves a stale partition table → the
        bootloader loads the wrong binary → silent failure.
        """
        result = self.run_esptool(
            "--after", "hard-reset", "write-flash",
            str(self.flash_offsets["bootloader"]), str(bootloader),
            str(self.flash_offsets["partition_table"]), str(partition_table),
            str(self.flash_offsets["ota_0_app"]), str(app),
        )
        return "Hash of data verified" in (result.stdout or "")

    def flash_wallet(self) -> bool:
        """Flash the known-good wallet firmware (reference/test oracle).

        Uses the wallet's OWN partition table (factory app at 0x30000).
        After this, flash_full() must be used to switch back to our scheme.
        """
        bl = self.wallet_fw_dir / "bootloader" / "bootloader.bin"
        pt = self.wallet_fw_dir / "partition_table" / "partition-table.bin"
        app = self.wallet_fw_dir / "nucula.bin"
        for f in [bl, pt, app]:
            if not f.exists():
                raise FileNotFoundError(f"Wallet firmware not found: {f}")
        result = self.run_esptool(
            "--after", "hard-reset", "write-flash",
            "0x0", str(bl),
            "0x8000", str(pt),
            "0x30000", str(app),
        )
        return "Hash of data verified" in (result.stdout or "")

    def full_erase(self) -> bool:
        """Erase entire flash. DANGEROUS — board needs USB replug after."""
        result = self.run_esptool("erase-flash", timeout=120)
        return result.returncode == 0

    # ------------------------------------------------------------------
    # Reset + verified boot (root-cause fix for the stale-firmware bug)
    # ------------------------------------------------------------------
    #
    # 2026-10-07 bench incident: esptool flashed a new image ("Hash of
    # data verified. Hard resetting via RTS pin...") but the board NEVER
    # reset — the health counter continued 52→124 across the flash and
    # the "new firmware" test silently exercised the OLD binary. The C3
    # USB-JTAG peripheral drops reset requests intermittently (observed
    # for both esptool's CDC control request and raw setRTS toggles on
    # an open port). openocd's JTAG reset has been 100% reliable.
    #
    # Protocol: flash with --after no-reset, JTAG-reset ourselves, then
    # PROVE which firmware booted before any test runs.

    def reset_jtag(self, timeout: int = 30) -> bool:
        """Reset the chip via the USB-JTAG tap (path is independent of CDC).

        Do NOT pipe openocd output through a shell pipe in the same
        command that waits on it — that hangs the invoking shell.
        """
        try:
            r = subprocess.run(
                [OPENOCD, "-f", "board/esp32c3-builtin.cfg",
                 "-c", "init; reset run; shutdown"],
                capture_output=True, timeout=timeout,
            )
            return r.returncode == 0 and b"tap/device found" in (r.stdout + r.stderr)
        except (subprocess.TimeoutExpired, FileNotFoundError):
            return False

    def flash_image(self, regions: dict, after: str = "hard-reset") -> None:
        """write-flash. `after` is a global esptool option (must precede
        the subcommand — esptool v5.3.1 rejects it after write-flash).

        The post-flash reset is verified by flash_and_boot(); 'no-reset'
        alone is NOT safe — it leaves the USB download-latch set and the
        chip boots back into download mode even after a JTAG core reset.
        """
        args = ["--after", after, "write-flash"]
        for off in sorted(regions):
            args += [hex(off), str(regions[off])]
        result = self.run_esptool(*args)
        if "Hash of data verified" not in (result.stdout or ""):
            raise RuntimeError(
                f"flash failed rc={result.returncode} "
                f"stderr_tail={(result.stderr or '')[-300:]}"
            )

    def _our_partition_table(self) -> Path:
        pt = self._esp_idf_sys_out() / "build" / "partition_table" / "partition-table.bin"
        if not pt.exists():
            raise FileNotFoundError(f"Partition table not found: {pt}")
        return pt

    def flash_and_boot(self, binary: Optional[Path] = None, *,
                       full: bool = False, wallet: bool = False,
                       expect: Optional[list[str]] = None,
                       boot_timeout: float = 30.0) -> str:
        """Flash → JTAG reset → PROVE the expected firmware booted.

        Returns the captured boot console text. Raises RuntimeError with
        console evidence if the board keeps running stale firmware.

        `expect` defaults per image type (FWID line for ours, prompt for
        the wallet). Stale-firmware detection: a health/probe counter of
        10+ seen without any expected marker means the old boot survived.
        """
        if wallet:
            bl = self.wallet_fw_dir / "bootloader" / "bootloader.bin"
            pt = self.wallet_fw_dir / "partition_table" / "partition-table.bin"
            app = self.wallet_fw_dir / "nucula.bin"
            for f in [bl, pt, app]:
                if not f.exists():
                    raise FileNotFoundError(f"Wallet firmware not found: {f}")
            self.flash_image({0x0: bl, 0x8000: pt, 0x30000: app})
            if expect is None:
                expect = ["nucula>"]
        else:
            assert binary is not None, "binary required unless wallet=True"
            if not binary.exists():
                raise FileNotFoundError(f"App binary not found: {binary}")
            pt = self._our_partition_table()
            if full:
                bl_dir = self._esp_idf_sys_out() / "build" / "bootloader"
                bl = next(bl_dir.glob("bootloader.bin"), None)
                if bl is None:
                    raise FileNotFoundError(
                        f"bootloader.bin not found under {bl_dir}"
                    )
                self.flash_image({0x0: bl, 0x8000: pt, 0x40000: binary})
            else:
                self.flash_image({0x8000: pt, 0x40000: binary})
            # Erase otadata after writing ota_0 (Codex PR-86 P1): the
            # wallet app is flashed at 0x30000 — right over our otadata
            # slot — so a wallet→ours switch leaves garbage otadata that
            # can steer the bootloader at ota_1. Erased otadata falls
            # back to ota_0 deterministically.
            r = self.run_esptool(
                "erase-region",
                hex(self.flash_offsets["otadata"]), "0x2000",
            )
            if r.returncode != 0:
                raise RuntimeError(
                    f"otadata erase failed rc={r.returncode} "
                    f"stderr_tail={(r.stderr or '')[-300:]}"
                )
            self._record_app_region_hash(binary)
            if expect is None:
                expect = ["FWID"]

        try:
            from .console import ConsoleCapture, BootMarker
        except ImportError:
            from console import ConsoleCapture, BootMarker  # type: ignore

        def _verify(cap: "ConsoleCapture", timeout: float) -> tuple[bool, str]:
            text = cap.capture_for(timeout, markers=expect)
            if all(m in text for m in expect):
                fw = BootMarker.identify_firmware(text)
                print(f"[flash_and_boot] booted: {fw}")
                return True, text
            if "waiting for download" in text or "DOWNLOAD" in text:
                print("[flash_and_boot] chip stuck in download mode")
            m = re.search(r"health\[(\d+)\]", text)
            if m and int(m.group(1)) >= 10:
                print(f"[flash_and_boot] STALE FIRMWARE "
                      f"(health[{m.group(1)}], expected marker missing)")
            return False, text

        last_text = ""
        # Reset ladder, most-reliable-first. esptool's hard-reset both
        # clears the download latch and resets (usually works); the JTAG
        # core reset fixes the silently-dropped-reset case (old firmware
        # still running); a flash-id round-trip recovers a chip latched
        # in download mode.
        #
        # The capture is OPENED before each reset, not after (Codex
        # PR-86 P1): boot output in the reset window must not be lost
        # to capture-open latency. ConsoleCapture deasserts DTR/RTS on
        # connect, so holding the fd across a reset cannot latch
        # download mode. It must be CLOSED before any esptool rung —
        # run_esptool frees the port with fuser, which would kill this
        # process if it held the fd.
        cap = ConsoleCapture(self.port, self.baud_console)
        cap.connect()
        ok, last_text = _verify(cap, boot_timeout)
        cap.close()
        if not ok:
            cap = ConsoleCapture(self.port, self.baud_console)
            cap.connect()
            self.reset_jtag()
            ok, last_text = _verify(cap, boot_timeout)
            cap.close()
        if not ok:
            # esptool needs exclusive port access; its --after
            # hard-reset fires at process exit, so the capture reopens
            # immediately on return, while the chip is still booting.
            self.run_esptool("--after", "hard-reset", "flash-id", timeout=30)
            cap = ConsoleCapture(self.port, self.baud_console)
            cap.connect()
            ok, last_text = _verify(cap, boot_timeout)
            cap.close()
        if not ok:
            raise RuntimeError(
                f"board did not boot expected firmware after reset ladder. "
                f"expect={expect} console_tail={last_text[-400:]!r}"
            )
        return last_text

    # ------------------------------------------------------------------
    # Pre/post-test checklists
    # ------------------------------------------------------------------

    def pretest_check(self) -> list[tuple[str, bool]]:
        """Fast sanity checklist BEFORE flashing/running tests.

        Every item must pass; failures here are cheap to diagnose.
        """
        results = [
            ("port exists", self._port_exists()),
            ("no external port holders", self._port_holders() == []),
            ("esptool responsive", self.get_flash_id() is not None),
        ]
        # Identify (or back up) whatever sits in the ota_0 app slot
        # BEFORE any HIL flash may overwrite it (Codex PR-86 P1,
        # m5stick incident: never destroy unidentifiable firmware).
        label, ok = self.check_app_region_known()
        results.append((label, ok))
        return results

    def _peek_app_region(self) -> Optional[bytes]:
        """Read the first 4KB of the ota_0 app slot (None if unreadable)."""
        raw = self.flash_backup_dir / "nucula-app-peek.bin"
        r = self.run_esptool(
            "read-flash",
            hex(self.flash_offsets["ota_0_app"]),
            hex(self.app_peek_size), str(raw),
        )
        if r.returncode != 0:
            return None
        try:
            return raw.read_bytes()
        except OSError:
            return None

    def _known_app_hashes(self) -> set[str]:
        """Hashes of images we may find at 0x40000 without backing up.

        Sources: the persisted hash log (written after every flash_and_boot
        of our firmware) and the wallet image (flashed at 0x30000, so the
        bytes at 0x40000 are its file offset 0x10000).
        """
        hashes: set[str] = set()
        if self.known_app_hashes_path.exists():
            lines = self.known_app_hashes_path.read_text().splitlines()
            hashes |= {l.split()[0] for l in lines if l.split()}
        wallet = self.wallet_fw_dir / "nucula.bin"
        if wallet.exists():
            data = wallet.read_bytes()
            off = self.flash_offsets["ota_0_app"] - self.wallet_app_base
            chunk = data[off:off + self.app_peek_size]
            if chunk:
                hashes.add(hashlib.sha256(chunk).hexdigest())
        return hashes

    def _record_app_region_hash(self, binary: Path) -> None:
        """Remember the head of the image we just flashed at ota_0."""
        try:
            head = binary.read_bytes()[: self.app_peek_size]
            self.flash_backup_dir.mkdir(parents=True, exist_ok=True)
            with open(self.known_app_hashes_path, "a") as f:
                f.write(f"{hashlib.sha256(head).hexdigest()} ours {binary.name}\n")
        except OSError:
            pass

    def check_app_region_known(self) -> tuple[str, bool]:
        """Identify the app region, backing it up first if unknown.

        Returns (label, ok). ok=False blocks HIL flashes: either the
        region is unreadable, or it holds unknown firmware AND the full
        4MB backup failed — destroying it anyway is how shared-bench
        boards get soft-bricked.
        """
        peek = self._peek_app_region()
        if peek is None:
            return ("app region readable", False)
        if all(b == 0xFF for b in peek):
            return ("app region erased", True)
        if hashlib.sha256(peek).hexdigest() in self._known_app_hashes():
            return ("app region identified", True)
        stamp = time.strftime("%Y%m%d-%H%M%S")
        backup = self.flash_backup_dir / f"nucula-unknown-fw-{stamp}.bin"
        r = self.run_esptool(
            "read-flash", "0x0", "0x400000", str(backup), timeout=600,
        )
        if r.returncode != 0:
            return (
                f"unknown app region, backup FAILED rc={r.returncode} "
                f"({(r.stderr or '')[-200:]}). Resolve before flashing",
                False,
            )
        # Record the backed-up hash so a repeated pretest recognizes it.
        try:
            self.flash_backup_dir.mkdir(parents=True, exist_ok=True)
            with open(self.known_app_hashes_path, "a") as f:
                f.write(f"{hashlib.sha256(peek).hexdigest()} backed-up-{stamp}\n")
        except OSError:
            pass
        print(f"[pretest] unknown firmware backed up: {backup}")
        return (f"unknown app region backed up: {backup.name}", True)

    def _port_holders(self) -> list[str]:
        try:
            r = subprocess.run(
                ["fuser", os.path.realpath(self.port_path)],
                capture_output=True, text=True, timeout=5,
            )
            return [p for p in (r.stdout or "").split() if p.isdigit()]
        except (subprocess.TimeoutExpired, FileNotFoundError):
            return []

    def ensure_responsive(self) -> bool:
        """Recovery ladder (no replug needed): port holders → JTAG reset."""
        for round_ in range(3):
            self._free_port()
            if self._port_exists() and self.get_flash_id() is not None:
                return True
            self.reset_jtag()
            time.sleep(3)
        return False

    def restore_known_good(self) -> bool:
        """Last-resort: flash the wallet firmware (the bench oracle).

        Use when our firmware leaves the board in an unknown state and
        the next test needs a trusted baseline.
        """
        try:
            self.flash_and_boot(wallet=True, boot_timeout=40.0)
            return True
        except (RuntimeError, FileNotFoundError) as e:
            print(f"[restore_known_good] FAILED: {e}")
            return False

    def reset_board(self) -> bool:
        """Soft-reset the board via RTS pulse (doesn't always work)."""
        try:
            import serial
            s = serial.Serial(self.port_path, self.baud_console, timeout=1)
            s.setRTS(True)
            time.sleep(0.2)
            s.setRTS(False)
            s.close()
            return True
        except Exception:
            return False

    def get_flash_id(self) -> Optional[str]:
        """Read chip MAC. Returns None if unresponsive."""
        try:
            result = self.run_esptool("--no-stub", "flash-id", timeout=30)
            for line in (result.stdout or "").split("\n"):
                if "MAC:" in line:
                    return line.split("MAC:")[1].strip()
        except (subprocess.TimeoutExpired, FileNotFoundError):
            pass
        return None

    def is_responsive(self) -> bool:
        """Check port exists AND board responds to esptool."""
        if not self._port_exists():
            return False
        return self.get_flash_id() is not None

    def build_firmware(self, features: str = "pn7160-bringup,pn7160-verdict-b") -> Optional[Path]:
        """Build the firmware and return the path to the app binary."""
        env = os.environ.copy()
        # Codex #150: source the espup environment when the caller's
        # shell hasn't (PATH for ldproxy, LIBCLANG_PATH) — without it a
        # fresh-shell --test-firmware build fails in esp-idf-sys.
        export_esp = Path.home() / "export-esp.sh"
        if env.get("LIBCLANG_PATH") is None and export_esp.is_file():
            for line in export_esp.read_text().splitlines():
                if line.startswith("export "):
                    k, _, v = line[len("export "):].partition("=")
                    env.setdefault(k.strip(), v.strip().strip('"'))
        env.update({
            # Pinned esp fork v1.98.1.0 — matches build.sh/CI (one-Rust-
            # version policy 2026-10-12: both esp32 targets ride the
            # pinned fork; the fork ships rust-src for build-std). The
            # alias is what espup installed at the pinned version; a
            # host that re-runs espup with another version builds wrong
            # (Codex #150 residual — version-check deliberately not
            # enforced here, documented in the runbook issue).
            "RUSTUP_TOOLCHAIN": "esp",
            "ESP_IDF_SDKCONFIG_DEFAULTS": f"{self.firmware_dir}/sdkconfig.wallet-test",
            "ESP_IDF_GLOB_PARTCSV_BASE": str(self.firmware_dir),
            "ESP_IDF_GLOB_PARTCSV_1": "/partitions-ota.csv",
            "CARGO_WORKSPACE_DIR": str(self.workspace_root),
            "IDF_CCACHE_ENABLE": "1",
        })
        env.pop("ESP_IDF_SDKCONFIG", None)

        result = subprocess.run(
            ["cargo", "build", "--target", "riscv32imc-esp-espidf",
             "--no-default-features", "--features", features],
            cwd=self.firmware_dir,
            env=env,
            capture_output=True,
            text=True,
            timeout=600,
        )
        if result.returncode != 0:
            # Loud failure: the captured build output is the only way to
            # diagnose env problems (missing LIBCLANG, sdkconfig cache
            # flips) — pytest -s shows it, the log capture keeps it.
            print("=== firmware build FAILED ===")
            print(result.stdout[-2000:])
            print(result.stderr[-4000:])
            return None

        elf = Path("/root/.cargo-target/riscv32imc-esp-espidf/debug/esp32-ccid")
        if not elf.exists():
            return None

        bin_path = Path("/tmp/opencode/test_flash.bin")
        conv = subprocess.run(
            ["esptool", "--chip", "esp32c3", "elf2image",
             "--output", str(bin_path), str(elf)],
            capture_output=True, text=True,
        )
        return bin_path if conv.returncode == 0 else None


# Singleton instance
BOARD = NuculaBoard()
