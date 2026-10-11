"""Pytest fixtures and tests for the nucula (ESP32-C3 + PN7160) board.

Usage:
    pytest tests/hardware/nucula/ -v --hil

    # Flash and test our bringup firmware:
    pytest tests/hardware/nucula/test_nucula.py -v --hil --test-firmware

    # Flash and test the wallet firmware (known-good reference):
    pytest tests/hardware/nucula/test_nucula.py -v --hil --test-wallet

    # Just check the board is responsive:
    pytest tests/hardware/nucula/test_nucula.py -v --hil -k test_board_responsive

All flashes go through board.flash_and_boot(): flash → boot-marker
verification (FWID for ours, prompt for the wallet) with a reset
ladder (JTAG reset, esptool flash-id round-trip) for dropped resets
and download-latch states — a flash that leaves stale firmware running
FAILS HERE instead of silently testing the wrong binary.
"""

import pytest
import time
from pathlib import Path

from .board import BOARD

# pyserial is a HARDWARE dep: imported lazily inside the fixtures so the
# no-hardware CI collection path (python-collect, minimal env) never
# needs it (AGENTS.md rule — hardware imports stay out of module scope).


def pytest_addoption(parser):
    parser.addoption("--test-firmware", action="store_true", default=False,
                     help="Flash and test our Rust bringup firmware")
    parser.addoption("--test-wallet", action="store_true", default=False,
                     help="Flash and test the wallet firmware (reference)")
    parser.addoption("--flash-binary", type=str, default=None,
                     help="Path to a specific binary to flash and test")



def pytest_collection_modifyitems(config, items):
    if not config.getoption("--hil"):
        skip_marker = pytest.mark.skip(reason="need --hil option to run")
        for item in items:
            if "hil" in item.keywords:
                item.add_marker(skip_marker)


@pytest.fixture(scope="session", autouse=True)
def pretest(board):
    """Pre-test checklist: fail fast (and cheap) if the board isn't sane."""
    failures = [name for name, ok in board.pretest_check() if not ok]
    assert not failures, (
        f"pre-test checklist failed: {failures}. "
        "Fix the bench state before running HIL tests."
    )
    yield
    # Post-test: board must still answer esptool; attempt recovery if not.
    if not board.ensure_responsive():
        board.restore_known_good()


@pytest.fixture(scope="session")
def board():
    """The nucula board instance."""
    return BOARD


@pytest.fixture(scope="session")
def console(board):
    from .console import ConsoleCapture
    """Console capture with fast USB-CDC reconnection."""
    capture = ConsoleCapture(board.port, board.baud_console)
    yield capture
    capture.close()


@pytest.fixture(scope="session")
def flashed_wallet(board):
    """Flash the wallet firmware (known-good) and VERIFY it booted.

    flash_and_boot JTAG-resets and requires the wallet prompt — a
    dropped RTS reset surfaces as a fixture failure, not stale tests.
    """
    board.flash_and_boot(wallet=True, boot_timeout=40.0)
    yield board


@pytest.fixture(scope="session")
def flashed_firmware(board, request):
    """Flash our Rust bringup firmware and VERIFY via FWID marker."""
    from .console import BootMarker
    # --flash-binary: prebuilt image instead of a rebuild (PR-86 P2).
    prebuilt = request.config.getoption("--flash-binary")
    if prebuilt is not None:
        binary = Path(prebuilt)
        assert binary.exists(), f"--flash-binary not found: {binary}"
    else:
        binary = board.build_firmware()
        assert binary is not None, "Firmware build failed"
    # full=True (idempotent): required after wallet firmware — its
    # bootloader boots factory@0x30000, never our ota_0@0x40000 (PR-86 P1).
    text = board.flash_and_boot(binary, full=True)
    fwid = BootMarker.parse_fwid(text)
    assert fwid is not None, f"No FWID marker in boot console: {text[-300:]}"
    print(f"[flashed_firmware] {fwid}")
    yield board


@pytest.fixture(scope="session")
def flashed_ccid_firmware(board):
    """Flash the pn7160-ccid reader firmware (USB-CDC CCID serve loop)
    from .console import BootMarker
    and VERIFY via FWID marker — the binary the dump-and-retrieve
    workflow (Escape 0xD1, AGENTS.md) is exercised against."""
    binary = board.build_firmware(features="pn7160-ccid")
    assert binary is not None, "Firmware build failed"
    text = board.flash_and_boot(binary, full=True)
    fwid = BootMarker.parse_fwid(text)
    assert fwid is not None, f"No FWID marker in boot console: {text[-300:]}"
    print(f"[flashed_ccid_firmware] {fwid}")
    yield board
