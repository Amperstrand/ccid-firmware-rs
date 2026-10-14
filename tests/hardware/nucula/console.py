"""Console capture for the nucula board — handles USB-CDC re-enumeration.

Solves the specific problems we've hit in manual testing:
- USB-CDC re-enumerates after flash → boot messages lost
- Serial port moves between ttyACM0 and ttyACM1
- Need to capture from BEFORE boot to AFTER test assertion

Uses the by-id path (follows the device regardless of tty number) and
reconnects aggressively during the re-enumeration window.
"""

# Lazy pyserial discipline (the python-collect CI gate runs with pytest +
# stdlib ONLY): this module is imported at collection time by
# test_nucula.py — `import serial` stays INSIDE the methods that use it,
# and the annotations stay lazy via `from __future__ import annotations`
# (the documented rule: hardware imports never at module scope of a
# collected file's import graph).
from __future__ import annotations

import re
import time
from typing import Optional


class ConsoleCapture:
    """Captures console output with fast reconnection for USB re-enumeration."""

    def __init__(self, port: str, baud: int = 115200):
        self.port = port
        self.baud = baud
        # _buffer: the current capture window (cleared at each
        # capture_for entry). _history: everything drained since this
        # capture was created, never cleared — one-shot boot markers
        # (e.g. 'NCI INIT LADDER OK') can land in an earlier test's
        # window, and per-window asserts never see them again
        # (Codex PR-86 P2).
        self._buffer = bytearray()
        self._history = bytearray()
        self._serial: Optional[serial.Serial] = None
        _ACTIVE_CAPTURES.append(self)

    def _connect(self) -> bool:
        """Try to open the serial port. Returns True on success.

        DTR/RTS are deasserted immediately after open: pyserial asserts
        DTR by default and the C3's USB-Serial/JTAG peripheral latches
        host line state across soft resets — a DTR-asserted holder turns
        every later reset into a boot:0x5 download-mode latch (AGENTS.md
        bench lessons, 2026-10-09/10).
        """
        import serial  # lazy: module is imported at collection time (CI gate)
        try:
            if self._serial and self._serial.is_open:
                return True
            self._serial = serial.Serial(self.port, self.baud, timeout=0.1)
            self._serial.setDTR(False)
            self._serial.setRTS(False)
            return True
        except (serial.SerialException, OSError):
            self._serial = None
            return False

    def connect(self) -> bool:
        """Open the serial fd now instead of lazily in capture_for.

        flash_and_boot opens the capture BEFORE issuing a reset so boot
        output is not lost to connect latency.
        """
        return self._connect()

    def _drain(self):
        """Read all available data into the buffer."""
        import serial  # lazy: module is imported at collection time (CI gate)
        if not self._serial:
            return
        try:
            data = self._serial.read(65536)
            if data:
                self._buffer.extend(data)
                self._history.extend(data)
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

    def history_text(self) -> str:
        """Full console text since capture creation (never cleared).

        One-shot boot markers can land in an earlier test's capture
        window; asserting against history is immune to per-test buffer
        clears (Codex PR-86 P2)."""
        return self._history.decode("utf-8", errors="replace")

    def close(self):
        if self._serial and self._serial.is_open:
            self._serial.close()
        self._serial = None


# Live captures. board._free_port() suspends all of them before its
# `fuser -k`: an fd held by THIS process (the session console fixture
# holds the port across tests) turns the kill into a pytest self-kill —
# empirically SIGKILL, rc 137 (Codex PR-86 P2). Captures reconnect
# lazily on their next capture_for/send_line, so suspending is
# transparent to tests.
_ACTIVE_CAPTURES: list[ConsoleCapture] = []


def suspend_all_captures() -> None:
    """Close every live capture's fd (they reconnect lazily)."""
    for cap in _ACTIVE_CAPTURES:
        cap.close()


class BootMarker:
    """Known boot markers for identifying which firmware is running."""

    # Compile-time identity — every Rust firmware logs this FIRST.
    # Format: FWID <name> rev=<git-short> build=<unix-ts>
    FWID = "FWID"
    FWID_RE = re.compile(r"FWID (\S+) rev=(\S+) build=(\d+)")

    # Our firmware markers (legacy, pre-FWID builds)
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
    # Mid-session health heartbeat (HEARTBEAT_PROBE_EVERY = 60 s):
    # success and failure twins — "still" vs "stopped", no overlap.
    PN7160_STILL_ACKING = "PN7160 still ACKing"
    PN7160_STOPPED_ACKING = "PN7160 stopped ACKing"
    NCI_INIT_OK = "NCI INIT LADDER OK"
    KEYBOARD_ACK = "keyboard @0x20 rc=0"
    BUS_PRIMED = "bus primed"

    @classmethod
    def parse_fwid(cls, console_text: str) -> Optional[dict]:
        """Extract the FWID line (name/rev/build-ts) if present."""
        m = cls.FWID_RE.search(console_text)
        if m:
            return {"name": m.group(1), "rev": m.group(2), "build": m.group(3)}
        return None

    @classmethod
    def identify_firmware(cls, console_text: str) -> str:
        """Identify which firmware produced this console output."""
        fwid = cls.parse_fwid(console_text)
        if fwid:
            return f"{fwid['name']}@{fwid['rev']}"
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
