"""HIL tests for the nucula (ESP32-C3 + PN7160) board.

Test pyramid (bottom to top):
1. test_board_responsive — board is alive (esptool flash-id)
2. test_wallet_boots — known-good wallet firmware boots and NCI inits
3. test_wallet_i2c_scan — clean bus scan: 0x20, 0x28, 0x7C
4. test_our_firmware_boots — our Rust firmware boots with boot markers
5. test_our_firmware_pn7160_acks — PN7160 ACKs within N seconds
6. test_our_firmware_nci_init — NCI init ladder completes
7. test_our_firmware_sustained_acks — sustained ACKs over 30s
"""

import re
import subprocess
import time

import pytest

from .console import ConsoleCapture, BootMarker


@pytest.mark.hil
def test_board_responsive(board):
    """Board USB port exists (doesn't open it — avoids port conflicts)."""
    import os
    assert os.path.exists(board.port), (
        f"USB port not found: {board.port}. Is the board plugged in?"
    )


@pytest.mark.hil
def test_wallet_boots(flashed_wallet, console):
    """Wallet firmware boots: console responds to 'status' with nfc: idle."""
    console._buffer.clear()
    time.sleep(20)  # wait for WiFi timeout + NFC init (~15s)
    console.send_line("status")
    text = console.capture_for(10, markers=[BootMarker.WALLET_NFC_IDLE])
    assert BootMarker.WALLET_NFC_IDLE in text, (
        f"Wallet NCI init failed. Expected 'nfc: idle'. Got: {text[-500:]}"
    )


@pytest.mark.hil
def test_wallet_i2c_scan(flashed_wallet, console):
    """Wallet firmware's i2cscan finds exactly 0x20, 0x28, 0x7C."""
    console._buffer.clear()
    console.send_line("i2cscan")
    text = console.capture_for(15, markers=["scan done"])
    acks = re.findall(r"ACK 0x([0-9A-Fa-f]{2})", text)
    acks_set = {a.upper() for a in acks}
    assert "20" in acks_set, f"Keyboard 0x20 not found. Scan: {text[-300:]}"
    assert "28" in acks_set, f"PN7160 0x28 not found. Scan: {text[-300:]}"
    assert "7C" in acks_set, f"Device 0x7C not found. Scan: {text[-300:]}"


@pytest.mark.hil
@pytest.mark.skipif(
    not pytest.config.getoption("--test-firmware", default=False)
    if hasattr(pytest, "config") else True,
    reason="need --test-firmware to flash our Rust firmware"
)
def test_our_firmware_pn7160_acks(flashed_firmware, console):
    """Our Rust firmware gets sustained ACKs from the PN7160."""
    console._buffer.clear()
    text = console.capture_for(60, markers=[BootMarker.PN7160_ACK])
    ack_count = text.count(BootMarker.PN7160_ACK)
    nak_count = text.count(BootMarker.PN7160_NAK)
    assert ack_count > 0, (
        f"PN7160 never ACKed in 60s. NAKs: {nak_count}. Console: {text[-500:]}"
    )


@pytest.mark.hil
@pytest.mark.skipif(
    not pytest.config.getoption("--test-firmware", default=False)
    if hasattr(pytest, "config") else True,
    reason="need --test-firmware to flash our Rust firmware"
)
def test_our_firmware_nci_init(flashed_firmware, console):
    """NCI init ladder completes (full PN7160 bring-up)."""
    console._buffer.clear()
    text = console.capture_for(60, markers=[BootMarker.NCI_INIT_OK])
    assert BootMarker.NCI_INIT_OK in text, (
        f"NCI init ladder did not complete. Console: {text[-500:]}"
    )


@pytest.mark.hil
@pytest.mark.skipif(
    not pytest.config.getoption("--test-firmware", default=False)
    if hasattr(pytest, "config") else True,
    reason="need --test-firmware to flash our Rust firmware"
)
def test_our_firmware_sustained_acks(flashed_firmware, console):
    """Sustained ACKs for 30 seconds (no flakiness)."""
    console._buffer.clear()
    text = console.capture_for(30)
    ack_count = text.count(BootMarker.PN7160_ACK)
    nak_count = text.count(BootMarker.PN7160_NAK)
    # Allow a few initial NAKs during boot/settling, but require sustained ACKs
    assert ack_count >= 5, f"Only {ack_count} ACKs in 30s. NAKs: {nak_count}"
    assert nak_count <= 5, f"Too many NAKs ({nak_count}) — unstable. ACKs: {ack_count}"


# --- Dump-and-retrieve workflow (Escape 0xD1, AGENTS.md) ---

SNAPSHOT_ELF = "/root/.cargo-target/riscv32imc-esp-espidf/debug/esp32-ccid"
COREDUMP_OFFSET = "0x3F0000"
COREDUMP_SIZE = "0x10000"


def _load_coredump_helpers():
    import importlib.util
    import pathlib
    helpers = pathlib.Path(__file__).resolve().parent.parent / "esp32_coredump.py"
    spec = importlib.util.spec_from_file_location("esp32_coredump", helpers)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


@pytest.mark.hil
def test_escape_d1_snapshot_roundtrip(flashed_ccid_firmware):
    """Dump-and-retrieve regression test (AGENTS.md "Crash Dumps & Snapshot
    Debugging"): Escape 0xD1 over the CCID wire → ack → silent flash
    coredump → reboot → host-side decode yields the panic marker frames.
    The coredump partition is erased before AND after, so the test both
    starts and leaves a clean bench state."""
    import serial as pyserial

    board = flashed_ccid_firmware
    coredump = _load_coredump_helpers()

    board.run_esptool("erase-region", COREDUMP_OFFSET, COREDUMP_SIZE)

    board._free_port()
    with pyserial.Serial(board.port, 115200, timeout=0.5) as s:
        s.reset_input_buffer()
        s.write(coredump.gempc_frame(b"\xD1"))
        wire = b""
        deadline = time.time() + 4
        while time.time() < deadline:
            wire += s.read(4096)
            if b"\xd1" in wire[2:] and bytes([0x83]) in wire:
                break
    echo = bytes([0x03, 0x06, 0x6B]) in wire
    ack = bytes([0x03, 0x06, 0x83]) in wire and b"\xD1" in wire
    assert echo and ack, (
        f"Escape 0xD1 not acked on the wire: {wire.hex()}"
    )

    time.sleep(8)  # coredump write + silent reboot

    raw = "/tmp/opencode/nucula-hil-coredump.bin"
    board.run_esptool("read-flash", COREDUMP_OFFSET, COREDUMP_SIZE, raw)
    data = open(raw, "rb").read()
    non_ff = any(chunk != b"\xff" * len(chunk) for chunk in (data[i:i+256] for i in range(0, len(data), 256)))
    assert non_ff, "coredump partition is empty — panic did not write a dump"

    decode = subprocess.run(
        [coredump.find_idf_python(), coredump.find_espcoredump_py(),
         "--chip", "esp32c3", "info_corefile", "--core", raw, "--core-format", "raw",
         "--gdb", "gdb-multiarch", SNAPSHOT_ELF],
        capture_output=True, text=True, timeout=180,
    )
    out = decode.stdout + decode.stderr
    # esp_coredump 5.5.1's thread printer crashes on newer gdb-multiarch
    # 'LWP N' thread ids AFTER emitting the panic info — tolerate the
    # nonzero exit as long as the essentials decoded.
    assert "Panic reason" in out, f"no panic reason in decode output: {out[-500:]}"
    assert "abort() was called at PC" in out, (
        f"panic reason is not an abort (snapshot panics via abort): {out[-500:]}"
    )
    assert "'main'" in out, f"crashed task is not main: {out[-500:]}"

    board.run_esptool("erase-region", COREDUMP_OFFSET, COREDUMP_SIZE)


@pytest.mark.hil
def test_log_shim_post_claim_output(flashed_ccid_firmware):
    """Issue #91 regression: after the USB-CDC driver claim, log lines
    must still reach the wire (ring-drained by the serving loop) and
    framed CCID commands must get LRC-valid answers amid the log text.

    Capture discipline (AGENTS.md "Session Lessons: Log-Shim Bench
    Verify"): one serial session, DTR/RTS held low (a DTR-asserted
    holder latches boot:0x5 download mode across resets), RTS pulse for
    a fresh boot, aggressive reconnect through the CDC re-enumeration
    (the reset kills our own fd)."""
    import serial as pyserial

    board = flashed_ccid_firmware
    board._free_port()

    def open_clean():
        s = pyserial.Serial(board.port, 115200, timeout=0.1)
        s.setDTR(False)
        s.setRTS(False)
        return s

    s = open_clean()
    s.reset_input_buffer()
    s.setRTS(True)
    time.sleep(0.2)
    s.setRTS(False)

    buf = bytearray()
    deadline = time.time() + 14
    while time.time() < deadline:
        try:
            data = s.read(4096)
            if data:
                buf.extend(data)
        except (pyserial.SerialException, OSError):
            try:
                s.close()
            except Exception:
                pass
            s = None
            while s is None and time.time() < deadline:
                try:
                    s = open_clean()
                except (pyserial.SerialException, OSError):
                    s = None
                    time.sleep(0.05)
    text = bytes(buf).decode("utf-8", "replace")

    assert "FWID pn7160-ccid" in text, (
        f"pre-claim FWID missing from boot console: {text[-300:]!r}"
    )
    assert "USB-CDC ready" in text, (
        f"post-claim line not ring-drained (issue #91 regression): "
        f"{text[-300:]!r}"
    )
    assert any(
        marker in text for marker in ("pn7160_i2c", "init attempt", "PN7160")
    ), f"PN7160 bring-up ladder not visible post-claim: {text[-300:]!r}"

    # Framed GetSlotStatus over the same open port: the response must be
    # SYNC-anchored and LRC-valid despite the interleaved log text.
    # Host line-state wiggling at reconnect can fire the C3's reset
    # magic — a second boot may still be running its ladder here, so
    # wait for the serve loop, then retry the frame a few times.
    gss = bytes([0x65, 0, 0, 0, 0, 0, 42, 0, 0, 0])
    frame = bytes([0x03, 0x06]) + gss
    lrc = 0
    for b in frame:
        lrc ^= b
    frame = frame + bytes([lrc])

    def read_available(deadline):
        got = bytearray()
        while time.time() < deadline:
            try:
                got += s.read(4096)
            except (pyserial.SerialException, OSError):
                break
        return bytes(got)

    ready_deadline = time.time() + 8
    while time.time() < ready_deadline:
        tail = read_available(time.time() + 0.5)
        if b"CCID loop starting" in tail:
            break

    wire = bytearray()
    for _ in range(3):
        try:
            s.write(frame)
        except (pyserial.SerialException, OSError):
            pytest.fail("port died before the framed probe")
        wire += read_available(time.time() + 1.5)
        if bytes([0x03, 0x06, 0x81]) in wire:
            break
        time.sleep(0.5)
    try:
        s.close()
    except Exception:
        pass

    needle = bytes([0x03, 0x06, 0x81])
    valid = False
    stream = bytes(wire)
    i = stream.find(needle)
    while i >= 0 and not valid:
        end = i + 13  # SYNC+CTRL+10-byte CCID header+LRC
        if end <= len(stream):
            x = 0
            for b in stream[i:end]:
                x ^= b
            valid = x == 0
        i = stream.find(needle, i + 1)
    assert valid, (
        f"no LRC-valid GetSlotStatus answer amid log text: {stream.hex()}"
    )
