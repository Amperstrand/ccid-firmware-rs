"""Console capture for the nucula board — handles USB-CDC re-enumeration.

Solves the specific problems we've hit in manual testing:
- USB-CDC re-enumerates after flash → boot messages lost
- Serial port moves between ttyACM0 and ttyACM1
- Need to capture from BEFORE boot to AFTER test assertion

Uses the by-id path (follows the device regardless of tty number) and
reconnects aggressively during the re-enumeration window.
"""

import re
import time
from pathlib import Path
from typing import Callable, Optional

import serial


class ConsoleCapture:
    """Captures console output with fast reconnection for USB re-enumeration."""

    def __init__(self, port: str, baud: int = 115200):
        self.port = port
        self.baud = baud
        self._buffer = bytearray()
        self._serial: Optional[serial.Serial] = None

    def _connect(self) -> bool:
        """Try to open the serial port. Returns True on success."""
        try:
            if self._serial and self._serial.is_open:
                return True
            self._serial = serial.Serial(self.port, self.baud, timeout=0.1)
            return True
        except (serial.SerialException, OSError):
            self._serial = None
            return False

    def _drain(self):
        """Read all available data into the buffer."""
        if not self._serial:
            return
        try:
            data = self._serial.read(65536)
            if data:
                self._buffer.extend(data)
        except (serial.SerialException, OSError):
            self._serial = None

    def capture_for(self, duration_s: float, markers: list[str] | None = None) -> str:
        """Capture console output for `duration_s` seconds.

        If `markers` is provided, returns early once ALL markers are found.
        """
        self._buffer.clear()
        start = time.time()
        while time.time() - start < duration_s:
            self._connect()
            self._drain()
            if markers and self._check_markers(markers):
                break
            time.sleep(0.05)  # 50ms poll — fast enough for USB-CDC
        return self._buffer.decode("utf-8", errors="replace")

    def _check_markers(self, markers: list[str]) -> bool:
        text = self._buffer.decode("utf-8", errors="replace")
        return all(m in text for m in markers)

    def send_line(self, line: str):
        """Send a line to the console (for interactive commands)."""
        if self._connect():
            self._serial.write((line + "\n").encode())

    def find_pattern(self, pattern: str) -> Optional[re.Match]:
        """Search the captured buffer for a regex pattern."""
        text = self._buffer.decode("utf-8", errors="replace")
        return re.search(pattern, text)

    def close(self):
        if self._serial and self._serial.is_open:
            self._serial.close()
        self._serial = None


class BootMarker:
    """Known boot markers for identifying which firmware is running."""

    # Our firmware markers
    RUST_BRINGUP = "pn7160-bringup: rust main ALIVE"
    RUST_M1 = "M1: minimal bus + probe"
    RUST_M1v2 = "M1v2: bus + VEN + probe"
    RUST_NETLOG_PREFIX = "[no-ip WARN"

    # Wallet firmware markers
    WALLET_BOOT = "nucula: wifi failed, continuing offline"
    WALLET_PROMPT = "nucula>"
    WALLET_NFC_IDLE = "nfc:     idle"
    WALLET_NFC_OFF = "nfc:     off"

    # Success/failure markers
    PN7160_ACK = "ACK @0x28"
    PN7160_NAK = "no-ack"
    NCI_INIT_OK = "NCI INIT LADDER OK"
    KEYBOARD_ACK = "keyboard @0x20 rc=0"
    BUS_PRIMED = "bus primed"

    @classmethod
    def identify_firmware(cls, console_text: str) -> str:
        """Identify which firmware produced this console output."""
        if cls.RUST_BRINGUP in console_text:
            return "rust-bringup"
        elif cls.RUST_M1v2 in console_text or cls.RUST_M1 in console_text:
            return "rust-m1"
        elif cls.WALLET_PROMPT in console_text or cls.WALLET_BOOT in console_text:
            return "wallet"
        elif "ESP-ROM" in console_text:
            return "unknown (boot completed)"
        else:
            return "unknown (no boot markers)"
