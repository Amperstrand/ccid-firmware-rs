"""Pytest fixtures and tests for the nucula (ESP32-C3 + PN7160) board.

Usage:
    pytest tests/hardware/nucula/ -v --hil

    # Flash and test our bringup firmware:
    pytest tests/hardware/nucula/test_nucula.py -v --hil --test-firmware

    # Flash and test the wallet firmware (known-good reference):
    pytest tests/hardware/nucula/test_nucula.py -v --hil --test-wallet

    # Just check the board is responsive:
    pytest tests/hardware/nucula/test_nucula.py -v --hil -k test_board_responsive
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
    """Flash the wallet firmware (known-good) and verify boot."""
    # Close any console connections first
    result = board.flash_wallet()
    if not result:
        # Debug: show what went wrong
        debug_result = board.run_esptool(
            "write-flash", "--after", "hard-reset",
            "0x0", "/tmp/opencode/nucula-fw/build-551/bootloader/bootloader.bin",
            "0x8000", "/tmp/opencode/nucula-fw/build-551/partition_table/partition-table.bin",
            "0x30000", "/tmp/opencode/nucula-fw/build-551/nucula.bin",
        )
        pytest.fail(
            f"Wallet flash failed. "
            f"rc={debug_result.returncode} "
            f"stdout_tail={debug_result.stdout[-300:] if debug_result.stdout else 'none'} "
            f"stderr_tail={debug_result.stderr[-300:] if debug_result.stderr else 'none'}"
        )
    capture = ConsoleCapture(board.port, board.baud_console)
    text = capture.capture_for(25, markers=[BootMarker.WALLET_PROMPT])
    firmware = BootMarker.identify_firmware(text)
    assert firmware == "wallet", f"Expected wallet firmware, got: {firmware}. Console: {text[-300:]}"
    capture.close()
    yield board


@pytest.fixture(scope="session")
def flashed_firmware(board):
    """Flash our Rust bringup firmware and verify boot."""
    binary = board.build_firmware()
    assert binary is not None, "Firmware build failed"
    assert board.flash_app(binary), "Firmware flash failed"
    yield board
