"""Pytest fixtures for CCID firmware HIL testing on the bench host.

LABGRID DOCTRINE (2026-10-09 migration)
=======================================

1. **labgrid coordinates hardware** — every HIL session acquires the
   `stm32-ccid` place (or the DUT's place) from the coordinator before
   touching hardware and releases it on teardown. Concurrent sessions
   cannot race a device. Exporter config lives in-repo
   (`exporter-ai-legion-nfc.yaml`) and is deployed to the bench host's
   /etc/labgrid.
2. **pcscd owns logical reader access** — readers are selected by stable
   identity (the DUT firmware's USB serial, embedded in the pcscd name),
   NEVER by enumeration order. Ambiguity (0 or >1 matches) is an error
   with a diagnostic listing, not a silent wrong-reader test.
3. **Tests run ON the bench host** — helpers execute as local
   subprocesses. SSH is only reintroduced for genuinely remote benches
   (via labgrid SSHDriver).

DUT identity: the F469 firmware's Cherry ST-2xxx emulation reports USB
serial `ST2XXX-001`; pcscd renders it as
`Cherry GmbH SmartTerminal ST-2xxx (ST2XXX-001) 01 00`. The bench's
authentic readers (NR7101, OmniKey CardMan, ACR1252) can never match that
serial — our emulated identity and the reference devices are disjoint by
construction.

Usage:
  pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil
"""

import os
import re
import shlex
import subprocess
import time
from pathlib import Path

import pytest

PLACE = "stm32-ccid"
DUT_READER_SERIAL = "ST2XXX-001"
CHERRY_VID_PID = "046a:003e"
EXPECTED_ATR = "3B D5 18 FF 81 91 FE 1F C3 80 73 C8 21 10 0A"
DEFAULT_FLASH_BASE = "0x08000000"
USB_RESCAN_DELAY_S = 3
CARD_CMD_ATTEMPTS = 3

# ---------------------------------------------------------------- helpers

REMOTE_APDU_SCRIPT = r'''
import sys
from smartcard.System import readers
from smartcard.util import toBytes

SELECT = "ST2XXX-001"  # F469 DUT serial — identity, not enumeration order
apdu = sys.argv[1] if len(sys.argv) > 1 else "00A40000"
rs = [r for r in readers() if SELECT in str(r)]
if not rs:
    print("ERROR:NOREADER:" + "|".join(str(r) for r in readers()))
    sys.exit(1)
c = rs[0].createConnection()
c.connect()
data, sw1, sw2 = c.transmit(toBytes(apdu))
print(bytes(data).hex() + ":%02X%02X" % (sw1, sw2))
'''.strip()

REMOTE_ATR_SCRIPT = r'''
from smartcard.scard import *
hresult, hcontext = SCardEstablishContext(SCARD_SCOPE_USER)
hresult, all_readers = SCardListReaders(hcontext, [])
readers = [r for r in all_readers if "ST2XXX-001" in r]
if not readers:
    print("ERROR:NOREADER:" + "|".join(all_readers))
    sys.exit(1)
hresult, hcard, proto = SCardConnect(hcontext, readers[0], SCARD_SHARE_SHARED, SCARD_PROTOCOL_T1)
hresult, reader, state, protocol, atr = SCardStatus(hcard)
SCardDisconnect(hcard, SCARD_LEAVE_CARD)
SCardReleaseContext(hcontext)
print(" ".join("%02X" % b for b in atr))
'''.strip()

REMOTE_ESCAPE_SCRIPT = r'''
import sys
from smartcard.pcsc.PCSCPart10 import (
    getFeatureRequest, hasFeature, FEATURE_CCID_ESC_COMMAND, SCARD_CTL_CODE
)
from smartcard.scard import SCARD_SHARE_DIRECT, SCARD_LEAVE_CARD
from smartcard.System import readers

SELECT = "ST2XXX-001"  # F469 DUT serial
rs = [r for r in readers() if SELECT in str(r)]
if not rs:
    print("ERROR:NOREADER:" + "|".join(str(r) for r in readers()))
    sys.exit(1)
c = rs[0].createConnection()
c.connect(mode=SCARD_SHARE_DIRECT, disposition=SCARD_LEAVE_CARD)
try:
    features = getFeatureRequest(c)
    esc_ioctl = hasFeature(features, FEATURE_CCID_ESC_COMMAND)
    if esc_ioctl is None:
        esc_ioctl = SCARD_CTL_CODE(1)
    resp = c.control(esc_ioctl, [0xD0])
    print(bytes(resp).hex())
finally:
    c.disconnect()
'''.strip()


def _lg(*args: str, check: bool = True):
    """Run labgrid-client against the coordinator for a place."""
    return subprocess.run(
        ["labgrid-client", "-p", PLACE, *args],
        capture_output=True, text=True, timeout=30, check=check,
    )


# ---------------------------------------------------------------- options

def pytest_addoption(parser):
    parser.addoption("--hil", action="store_true", default=False,
                     help="Enable HIL tests (disabled by default — requires hardware).")
    parser.addoption("--firmware-bin", action="store", default=None,
                     help="Path to .bin to flash before tests. If omitted, run against current flash.")
    parser.addoption("--skip-labgrid", action="store_true", default=False,
                     help="Skip labgrid place acquisition (coordinator-outage escape hatch).")


def pytest_configure(config):
    config.addinivalue_line("markers", "hil: hardware-in-the-loop test (requires --hil flag)")


def pytest_collection_modifyitems(config, items):
    if not config.getoption("--hil"):
        skip_hil = pytest.mark.skip(reason="HIL test — pass --hil to run")
        for item in items:
            if "hil" in item.keywords:
                item.add_marker(skip_hil)


# ---------------------------------------------------------------- fixtures

@pytest.fixture(scope="session")
def labgrid_place(request):
    """Acquire the DUT's labgrid place for the whole session — the single
    coordination point for bench access (released on teardown).
    """
    if request.config.getoption("--skip-labgrid"):
        yield None
        return
    r = _lg("acquire")
    if r.returncode != 0:
        pytest.fail(
            f"could not acquire labgrid place '{PLACE}' "
            f"(already held? `labgrid-client -p {PLACE} who`): {r.stderr.strip()}"
        )
    print(f"[labgrid] acquired {PLACE}")
    yield PLACE
    _lg("release", check=False)
    print(f"[labgrid] released {PLACE}")


@pytest.fixture(scope="session")
def bench(labgrid_place):
    """Bench-host command runner. Tests run ON the bench host: helpers are
    local subprocesses (no SSH — that is only for genuinely remote benches).
    """
    class Bench:
        def run(self, cmd: str, timeout: int = 30) -> subprocess.CompletedProcess:
            return subprocess.run(cmd, shell=True, capture_output=True,
                                  text=True, timeout=timeout)

        def put_script(self, path: str, content: str):
            Path(path).write_text(content)

    return Bench()


HELPER_DIR = "/tmp/hil-ccid-bench"


@pytest.fixture(scope="session")
def helpers(bench):
    """Install helper scripts once per session (owner-scoped dir: the
    bench is multi-user root/ubuntu — a bare /tmp path collides across
    sessions with PermissionError)."""
    Path(HELPER_DIR).mkdir(parents=True, exist_ok=True)
    bench.put_script(f"{HELPER_DIR}/hil_apdu.py", REMOTE_APDU_SCRIPT)
    bench.put_script(f"{HELPER_DIR}/hil_atr.py", REMOTE_ATR_SCRIPT)
    bench.put_script(f"{HELPER_DIR}/hil_escape.py", REMOTE_ESCAPE_SCRIPT)
    yield bench


@pytest.fixture(scope="session")
def flashed_firmware(request, helpers):
    """Flash firmware .bin to the F469 via st-flash (SWD)."""
    bin_path = request.config.getoption("--firmware-bin")
    if bin_path is None:
        yield None
        return
    bin_path = Path(bin_path).resolve()
    if not bin_path.exists():
        pytest.fail(f"Firmware binary not found: {bin_path}")
    result = helpers.run(f"st-flash --reset write {shlex.quote(str(bin_path))} {DEFAULT_FLASH_BASE}", timeout=60)
    if result.returncode != 0:
        pytest.fail(f"st-flash failed:\nstdout: {result.stdout}\nstderr: {result.stderr}")
    time.sleep(USB_RESCAN_DELAY_S)
    yield str(bin_path)


@pytest.fixture(scope="session")
def cherry_reader(helpers, flashed_firmware):
    result = helpers.run("lsusb")
    assert CHERRY_VID_PID in result.stdout, (
        f"DUT ({CHERRY_VID_PID}) not found:\n{result.stdout}"
    )
    yield helpers


@pytest.fixture(scope="session")
def pcscd_running(bench):
    bench.run("systemctl start pcscd.socket pcscd.service")
    time.sleep(1)
    result = bench.run("systemctl is-active pcscd.socket")
    assert result.stdout.strip() == "active", f"pcscd.socket not active: {result.stdout}"
    yield bench


@pytest.fixture(scope="session")
def pcsc_reader_name(cherry_reader, pcscd_running):
    """The DUT's pcscd reader, selected by USB serial — the ONLY stable
    identity (enumeration order changes between boots; the bench carries
    five readers)."""
    result = cherry_reader.run(
        "python3 -c 'from smartcard.System import readers; "
        "rs=[r for r in readers() if \"ST2XXX-001\" in str(r)]; "
        "print(str(rs[0]) if rs else \"\")'"
    )
    name = result.stdout.strip()
    assert name, (
        f"DUT reader (serial {DUT_READER_SERIAL}) not in pcscd:\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    assert DUT_READER_SERIAL in name, f"Unexpected reader (serial missing): {name}"
    yield name


def remote_apdu(bench, apdu_hex: str, timeout: int = 15) -> tuple[bytes, int, int]:
    """Send one APDU via the pcscd helper. Contact cards flake (the bench
    ComSign eID is aged — SELECT times out ~1-in-3): retry with a fresh
    connection each attempt."""
    clean = apdu_hex.replace(" ", "").replace("\t", "")
    if not re.match(r'^[0-9A-Fa-f]+$', clean):
        pytest.fail(f"Invalid APDU hex: {apdu_hex!r}")
    last_err = None
    for attempt in range(1, CARD_CMD_ATTEMPTS + 1):
        try:
            result = bench.run(f"python3 {HELPER_DIR}/hil_apdu.py {shlex.quote(clean)}", timeout=timeout)
        except subprocess.TimeoutExpired:
            last_err = f"attempt {attempt}: timeout"
            continue
        if result.returncode == 0 and ":" in result.stdout:
            output = result.stdout.strip()
            data_hex, sw_hex = output.rsplit(":", 1)
            data = bytes.fromhex(data_hex) if data_hex else b""
            sw1, sw2 = int(sw_hex[:2], 16), int(sw_hex[2:4], 16)
            return data, sw1, sw2
        last_err = f"attempt {attempt}: rc={result.returncode} out={result.stdout.strip()[:120]}"
    pytest.fail(f"APDU {clean} failed after {CARD_CMD_ATTEMPTS} attempts ({last_err})")
