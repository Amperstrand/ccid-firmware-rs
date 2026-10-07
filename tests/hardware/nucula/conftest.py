"""Pytest fixtures and tests for the nucula (ESP32-C3 + PN7160) board.

Usage:
    pytest tests/hardware/nucula/ -v --hil

    # Flash and test our bringup firmware:
    pytest tests/hardware/nucula/test_nucula.py -v --hil --test-firmware

    # Flash and test the wallet firmware (known-good reference):
    pytest tests/hardware/nucula/test_nucula.py -v --hil --test-wallet

    # Just check the board is responsive:
    pytest tests/hardware/nucula/test_nucula.py -v --hil -k test_board_responsive

All flashes go through board.flash_and_boot(): no-reset flash + JTAG
reset + FWID/marker verification — a flash that leaves stale firmware
running FAILS HERE instead of silently testing the wrong binary.
"""

import pytest
import time

from .board import BOARD
from .console import ConsoleCapture, BootMarker


def pytest_addoption(parser):
    parser.addoption("--hil", action="store_true", default=False,
                     help="Run hardware-in-the-loop tests")
    parser.addoption("--test-firmware", action="store_true", default=False,
                     help="Flash and test our Rust bringup firmware")
    parser.addoption("--test-wallet", action="store_true", default=False,
                     help="Flash and test the wallet firmware (reference)")
    parser.addoption("--flash-binary", type=str, default=None,
                     help="Path to a specific binary to flash and test")


def pytest_configure(config):
    config.addinivalue_line("markers", "hil: hardware-in-the-loop test")


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
def flashed_firmware(board):
    """Flash our Rust bringup firmware and VERIFY via FWID marker."""
    binary = board.build_firmware()
    assert binary is not None, "Firmware build failed"
    text = board.flash_and_boot(binary)
    fwid = BootMarker.parse_fwid(text)
    assert fwid is not None, f"No FWID marker in boot console: {text[-300:]}"
    print(f"[flashed_firmware] {fwid}")
    yield board
