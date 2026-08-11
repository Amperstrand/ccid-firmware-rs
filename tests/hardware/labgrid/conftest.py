"""
Pytest fixtures for CCID firmware HIL testing on STM32F469I-DISCO.

The hardware testbed lives on 192.168.13.208:
  - STM32F469I-DISCO connected via ST-LINK/V2.1 (SWD flashing)
  - pcscd + pyscard installed for PC/SC verification
  - Contact smartcard (ComSign eID) in the slot

These fixtures wrap SSH calls to the remote host so that pytest
running on the build machine (.221) can flash firmware, check USB
enumeration, and send APDUs to the real reader.

Usage:
  pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil --ssh-host=192.168.13.208
"""

import os
import re
import shlex
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

import pytest

DEFAULT_SSH_HOST = "192.168.13.208"
DEFAULT_SSH_USER = "root"
DEFAULT_FLASH_BASE = "0x08000000"
USB_RESCAN_DELAY_S = 3

CHERRY_VID_PID = "046a:003e"
EXPECTED_ATR = "3B D5 18 FF 81 91 FE 1F C3 80 73 C8 21 10 0A"

REMOTE_APDU_SCRIPT = r'''
import sys
from smartcard.System import readers
from smartcard.util import toBytes

apdu = sys.argv[1] if len(sys.argv) > 1 else "00A40000"
rs = readers()
if not rs:
    print("ERROR:NOREADER")
    sys.exit(1)
c = rs[0].createConnection()
c.connect()
data, sw1, sw2 = c.transmit(toBytes(apdu))
print(bytes(data).hex() + ":%02X%02X" % (sw1, sw2))
'''.strip()

REMOTE_ATR_SCRIPT = r'''
from smartcard.scard import *
hresult, hcontext = SCardEstablishContext(SCARD_SCOPE_USER)
hresult, readers = SCardListReaders(hcontext, [])
if not readers:
    print("ERROR:NOREADER")
    sys.exit(1)
hresult, hcard, proto = SCardConnect(hcontext, readers[0], SCARD_SHARE_SHARED, SCARD_PROTOCOL_T1)
hresult, reader, state, protocol, atr = SCardStatus(hcard)
SCardDisconnect(hcard, SCARD_LEAVE_CARD)
SCardReleaseContext(hcontext)
print(" ".join("%02X" % b for b in atr))
'''.strip()


def pytest_addoption(parser):
    parser.addoption("--ssh-host", action="store",
                     default=os.environ.get("HIL_SSH_HOST", DEFAULT_SSH_HOST))
    parser.addoption("--ssh-user", action="store",
                     default=os.environ.get("HIL_SSH_USER", DEFAULT_SSH_USER))
    parser.addoption("--firmware-bin", action="store", default=None,
                     help="Path to .bin to flash before tests. If omitted, run against current flash.")
    parser.addoption("--hil", action="store_true", default=False,
                     help="Enable HIL tests (disabled by default — requires hardware).")


def pytest_configure(config):
    config.addinivalue_line("markers", "hil: hardware-in-the-loop test (requires --hil flag)")


def pytest_collection_modifyitems(config, items):
    if config.getoption("--hil"):
        return
    skip_hil = pytest.mark.skip(reason="HIL test — pass --hil to run")
    for item in items:
        if "hil" in item.keywords:
            item.add_marker(skip_hil)


@dataclass
class RemoteHost:
    host: str
    user: str

    @property
    def ssh_target(self) -> str:
        return f"{self.user}@{self.host}"

    def run(self, cmd: str, timeout: int = 30) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "ConnectTimeout=5",
             self.ssh_target, cmd],
            capture_output=True, text=True, timeout=timeout,
        )

    def scp_upload(self, local: str, remote: str, timeout: int = 30):
        subprocess.run(
            ["scp", "-o", "StrictHostKeyChecking=no", local,
             f"{self.ssh_target}:{remote}"],
            capture_output=True, text=True, timeout=timeout, check=True,
        )

    def put_script(self, remote_path: str, content: str):
        import tempfile
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as f:
            f.write(content)
            f.flush()
        try:
            self.scp_upload(f.name, remote_path)
        finally:
            os.unlink(f.name)


@pytest.fixture(scope="session")
def remote(request) -> RemoteHost:
    return RemoteHost(
        host=request.config.getoption("--ssh-host"),
        user=request.config.getoption("--ssh-user"),
    )


@pytest.fixture(scope="session")
def helpers(remote: RemoteHost):
    """Upload helper scripts to the remote host once per session."""
    remote.put_script("/tmp/hil_apdu.py", REMOTE_APDU_SCRIPT)
    remote.put_script("/tmp/hil_atr.py", REMOTE_ATR_SCRIPT)
    yield remote


@pytest.fixture(scope="session")
def flashed_firmware(request, helpers):
    """Flash firmware .bin to STM32 via st-flash over SSH."""
    bin_path = request.config.getoption("--firmware-bin")
    if bin_path is None:
        yield None
        return
    bin_path = Path(bin_path).resolve()
    if not bin_path.exists():
        pytest.fail(f"Firmware binary not found: {bin_path}")
    remote_bin = f"/tmp/{bin_path.name}"
    helpers.scp_upload(str(bin_path), remote_bin)
    result = helpers.run(f"st-flash --reset write {shlex.quote(remote_bin)} {DEFAULT_FLASH_BASE}", timeout=30)
    if result.returncode != 0:
        pytest.fail(f"st-flash failed:\nstdout: {result.stdout}\nstderr: {result.stderr}")
    time.sleep(USB_RESCAN_DELAY_S)
    yield str(bin_path)


@pytest.fixture(scope="session")
def cherry_reader(helpers, flashed_firmware):
    result = helpers.run("lsusb")
    assert CHERRY_VID_PID in result.stdout, (
        f"Cherry ST-2xxx ({CHERRY_VID_PID}) not found:\n{result.stdout}"
    )
    yield helpers


@pytest.fixture(scope="session")
def pcscd_running(remote: RemoteHost):
    remote.run("systemctl start pcscd.socket pcscd.service")
    time.sleep(1)
    result = remote.run("systemctl is-active pcscd.socket")
    assert result.stdout.strip() == "active", f"pcscd.socket not active: {result.stdout}"
    yield remote


@pytest.fixture(scope="session")
def pcsc_reader_name(cherry_reader, pcscd_running):
    result = cherry_reader.run(
        "python3 -c 'from smartcard.System import readers; "
        "rs=readers(); print(str(rs[0]) if rs else \"\")'"
    )
    name = result.stdout.strip()
    assert name, f"No PC/SC reader:\nstdout: {result.stdout}\nstderr: {result.stderr}"
    assert "Cherry" in name or "ST-2xxx" in name, f"Unexpected reader: {name}"
    yield name


def remote_apdu(remote: RemoteHost, apdu_hex: str, timeout: int = 10) -> tuple[bytes, int, int]:
    """Send APDU via remote pyscard helper. Returns (data, sw1, sw2)."""
    clean = apdu_hex.replace(" ", "").replace("\t", "")
    if not re.match(r'^[0-9A-Fa-f]+$', clean):
        pytest.fail(f"Invalid APDU hex: {apdu_hex!r}")
    result = remote.run(f"python3 /tmp/hil_apdu.py {shlex.quote(clean)}", timeout=timeout)
    if result.returncode != 0:
        pytest.fail(f"APDU relay failed:\nstdout: {result.stdout}\nstderr: {result.stderr}")
    output = result.stdout.strip()
    if ":" not in output:
        pytest.fail(f"Unexpected APDU output format: '{output}'")
    data_hex, sw_hex = output.rsplit(":", 1)
    data = bytes.fromhex(data_hex) if data_hex else b""
    sw1, sw2 = int(sw_hex[:2], 16), int(sw_hex[2:4], 16)
    return data, sw1, sw2
