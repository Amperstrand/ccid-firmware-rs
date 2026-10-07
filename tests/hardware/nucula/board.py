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

import os
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


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

    def __post_init__(self):
        self.firmware_dir = self.workspace_root / "firmware" / "esp32-ccid"
        target = Path("/root/.cargo-target/riscv32imc-esp-espidf")
        candidates = list(target.glob("debug/build/esp-idf-sys-*/out"))
        if candidates:
            self.esp_idf_sys_out = max(candidates, key=lambda p: p.stat().st_mtime)

    @property
    def port(self) -> str:
        return self.port_path

    def _free_port(self):
        """Aggressively free the serial port from any process holding it."""
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
        pt = self.esp_idf_sys_out / "build" / "partition_table" / "partition-table.bin"
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
        env.update({
            "RUSTUP_TOOLCHAIN": "nightly",
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
