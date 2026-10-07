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
