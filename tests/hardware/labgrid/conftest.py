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
import subprocess
import time
from pathlib import Path

import pytest

# Direct-name imports: this conftest also defines a `bench` FIXTURE (the
# host runner), which would shadow a module import of the same name.
from bench import (  # noqa: F401  (PLACES re-exported for addoption default)
    PLACES,
    BenchLockHeld,
    bench_lock as _bench_lock_ctx,
    bench_lock_mode,
    ledger as _ledger,
    place_holder as _place_holder,
    yield_acquired_place as _yield_acquired_place,
)

import stm32_flash  # noqa: E402  (sibling module, after the bench import block)

# APDU helpers live in hil_helpers.py (uniquely named): bare
# `from conftest import ...` in test modules breaks when several suite
# directories are collected together — whichever conftest.py claims the
# module name first wins. Same pattern as bench.py above.
from hil_helpers import (  # noqa: F401  (re-exported for fixture use)
    CARD_CMD_ATTEMPTS,
    EXPECTED_ATR,
    HELPER_DIR,
    remote_apdu,
)

PLACE = "stm32-ccid"
DUT_READER_SERIAL = "ST2XXX-001"
CHERRY_VID_PID = "046a:003e"
USB_RESCAN_DELAY_S = 3

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


def _reserve_wait(place: str, prio: int, timeout_s: int):
    """Queue for a place via a coordinator reservation (the polite
    cross-project pattern — conwrt-bench docs/labgrid-coordination.md).
    Blocks until allocated; returns the reservation token.

    Reservation, not poll-acquire: once the coordinator allocates, only
    this reservation's owner can lock the place — the current holder's
    release hands it over instead of a race.
    """
    r = subprocess.run(
        ["labgrid-client", "reserve", "--wait", "--prio", str(prio), f"name={place}"],
        capture_output=True, text=True, timeout=timeout_s, check=False,
    )
    if r.returncode != 0:
        pytest.fail(f"reservation for '{place}' failed: {(r.stderr or r.stdout).strip()[:200]}")
    m = re.search(r"^\s*token:\s*(\S+)", r.stdout, re.M)
    if not m:
        pytest.fail(f"could not parse reservation token from: {r.stdout!r}")
    return m.group(1)


def _lg_token(token: str, *args: str):
    """Run labgrid-client against the place allocated to a reservation."""
    return subprocess.run(
        ["labgrid-client", "-p", f"+{token}", *args],
        capture_output=True, text=True, timeout=30, check=False,
    )


# ---------------------------------------------------------------- options

def pytest_addoption(parser):
    # --hil is registered ONCE in tests/hardware/conftest.py (the shared
    # parent) — a second registration here kills combined-suite runs with
    # ValueError: option names {'--hil'} already added.
    parser.addoption("--firmware-bin", action="store", default=None,
                     help="Path to .bin to flash before tests. If omitted, run against current flash.")
    parser.addoption("--skip-labgrid", action="store_true", default=False,
                     help="Skip labgrid place acquisition (coordinator-outage escape hatch).")
    parser.addoption("--places", action="store", default=",".join(PLACES),
                     help="Comma-separated subset of bench places to cover "
                          "(stm32-ccid,nucula-c3,m5stick,ref-cardman; "
                          "ref-acr1252 retired 2026-10-25 — now "
                          "ref-acr1252-small on ai-legion-small).")
    parser.addoption("--reserve", action="store_true", default=False,
                     help="Queue for busy places via coordinator reservations "
                          "(reserve --wait) instead of failing/skipping — the "
                          "polite cross-project pattern (conwrt-bench "
                          "docs/labgrid-coordination.md).")
    parser.addoption("--reserve-priority", action="store", type=int, default=0,
                     help="Reservation priority (default 0; CI lanes use "
                          "negative to yield to interactive sessions).")
    parser.addoption("--reserve-timeout-min", action="store", type=int, default=120,
                     help="How long --reserve may block waiting for a place "
                          "(minutes, default 120).")
    parser.addoption("--acr-emu", action="store_true", default=False,
                     help="enable ACR1252 card-emulation HIL tests "
                          "(test_acr1252_emu_hil.py; tier-2 is xfail until "
                          "RF coupling is verified — see acr1252_emu.py STATUS)")


def pytest_collection_modifyitems(config, items):
    if not config.getoption("--hil"):
        skip_hil = pytest.mark.skip(reason="HIL test — pass --hil to run")
        for item in items:
            if "hil" in item.keywords:
                item.add_marker(skip_hil)
    # Hold identity (bench canon: sign every hold with who-and-why).
    # Shows in `labgrid-client who` as e.g.
    #   ccid-hil~places=m5stick~sha=3ab1e4a@<host>
    # Export LG_USERNAME yourself for richer text (project#issue~est=~...).
    if config.getoption("--hil") and not os.environ.get("LG_USERNAME"):
        try:
            sha = subprocess.run(
                ["git", "rev-parse", "--short=7", "HEAD"],
                capture_output=True, text=True, timeout=10, cwd=os.path.dirname(__file__),
            ).stdout.strip()
        except Exception:
            sha = "nogit"
        places = config.getoption("--places")
        os.environ["LG_USERNAME"] = f"ccid-hil~places={places}~sha={sha}"


# ---------------------------------------------------------------- fixtures

@pytest.fixture(autouse=True, scope="session")
def bench_lock_gate(request):
    """Codex #185: under --hil the flock gates the WHOLE invocation —
    test_ccid_hil drives pcscd without requesting bench_lock, so a
    per-test lock let its legs race concurrent serial sessions."""
    if not request.config.getoption("--hil"):
        yield None
        return
    request.getfixturevalue("bench_lock")
    yield


# The #334 extraction lives in bench.py (bench_lock_mode,
# yield_acquired_place) so host unit tests can import it by its unique
# module name — imported above as bench_lock_mode /
# _yield_acquired_place.


@pytest.fixture(scope="session")
def bench_lock(request):
    """Cross-project bench flock (bolty-rs protocol, layer 1) for tests
    that drive pcscd or serial ports. Held for the session.

    #334: --skip-labgrid skips only PLACE acquisition (the coordinator
    escape hatch) — the flock is still taken, so pcscd/serial legs keep
    their cross-project exclusion in every mode.
    """
    mode = bench_lock_mode(request.config.getoption("--skip-labgrid"))
    try:
        with _bench_lock_ctx():
            print(f"[bench] flock acquired (cross-project, mode={mode})")
            yield True
    except BenchLockHeld as e:
        pytest.skip(f"bench busy: {e}")


@pytest.fixture(params=list(PLACES))
def place_name(request):
    """Parametrized over the bench places selected via --places. Busy
    places SKIP (with the holder named) instead of failing a run — a
    shared bench yields partial coverage, visibly. With --reserve they
    QUEUE instead (owner-locked allocation once granted).
    """
    wanted = request.config.getoption("--places").split(",")
    if request.param not in wanted:
        pytest.skip(f"place not selected: {request.param}")
    if request.config.getoption("--skip-labgrid"):
        yield request.param
        return
    holder = _place_holder(request.param)
    if holder is None:
        pytest.fail(f"coordinator unreachable for place {request.param}")
    if holder and request.config.getoption("--reserve"):
        prio = request.config.getoption("--reserve-priority")
        timeout_s = request.config.getoption("--reserve-timeout-min") * 60
        token = _reserve_wait(request.param, prio, timeout_s)
        print(f"[labgrid] reservation allocated for {request.param} "
              f"(was held by {holder}; token {token})")
        r = _lg_token(token, "acquire")
        if r.returncode != 0:
            subprocess.run(["labgrid-client", "cancel-reservation", token],
                           capture_output=True, timeout=30)
            pytest.fail(f"could not acquire allocated place '{request.param}': "
                        f"{r.stderr.strip()[:200]}")
        _ledger("place_acquired", place=request.param, reserved=True)
        yield request.param
        _lg_token(token, "release")
        subprocess.run(["labgrid-client", "cancel-reservation", token],
                       capture_output=True, timeout=30)
        _ledger("place_released", place=request.param, reserved=True)
        return
    if holder:
        pytest.skip(f"place held by {holder}: {request.param}")
    # #334: a place lost between the holder check and the acquire is a
    # SKIP with the winning holder named — never a setup ERROR.
    yield from _yield_acquired_place(request.param, pytest.skip)


@pytest.fixture(scope="session")
def labgrid_place(request):
    """Acquire the DUT's labgrid place for the whole session — the single
    coordination point for bench access (released on teardown).

    With --reserve: queue via a coordinator reservation instead of
    failing when the place is busy (anti-bounce — the allocation is
    owner-locked once granted).
    """
    if request.config.getoption("--skip-labgrid"):
        yield None
        return
    token = None
    if request.config.getoption("--reserve"):
        prio = request.config.getoption("--reserve-priority")
        timeout_s = request.config.getoption("--reserve-timeout-min") * 60
        token = _reserve_wait(PLACE, prio, timeout_s)
        print(f"[labgrid] reservation allocated for {PLACE} (token {token})")
        r = _lg_token(token, "acquire")
        if r.returncode != 0:
            subprocess.run(["labgrid-client", "cancel-reservation", token],
                           capture_output=True, timeout=30)
            pytest.fail(f"could not acquire allocated place '{PLACE}': {r.stderr.strip()[:200]}")
        print(f"[labgrid] acquired {PLACE} via reservation")
        yield PLACE
        _lg_token(token, "release")
        subprocess.run(["labgrid-client", "cancel-reservation", token],
                       capture_output=True, timeout=30)
        print(f"[labgrid] released {PLACE}, reservation cancelled")
        return
    r = _lg("acquire", check=False)
    inherited = "You have already acquired" in (r.stderr or "")
    if r.returncode != 0 and not inherited:
        # A held or unreachable place is a SKIP, never a FAIL (#183
        # SKIP≠FAIL): one busy place must cost its own legs, not seven
        # setup errors — and a bench-infra outage is amber, not red.
        holder = _place_holder(PLACE)
        if holder:
            pytest.skip(f"{PLACE} held by {holder} — its legs are theirs")
        pytest.skip(f"{PLACE} not acquirable (bench/coordinator issue): "
                    f"{(r.stderr or '').strip()[:160]}")
    if inherited:
        # Shared-identity bench: the hold may be a sibling session's — the
        # coordination goal is met, and teardown must not release it.
        print(f"[labgrid] place {PLACE} already acquired by our identity — inheriting")
    else:
        print(f"[labgrid] acquired {PLACE}")
    yield PLACE
    if not inherited:
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
    """Flash firmware .bin to the F469 via st-flash (SWD).

    The ST-LINK adapter is pinned by serial, resolved from the in-repo
    exporter config — bare local st-flash targets whatever ST-LINK is
    attached, which crossed boards on the shared bench (2026-10-14: the
    F469's ST-LINK was detached, the F746's was attached, and an F469
    image landed on the F746)."""
    bin_path = request.config.getoption("--firmware-bin")
    if bin_path is None:
        yield None
        return
    import bench as benchmod

    absent = benchmod.dut_absent_reason("stm32-ccid")
    if absent:
        pytest.skip(f"stm32-ccid: flash skipped — {absent}")
    bin_path = Path(bin_path).resolve()
    if not bin_path.exists():
        pytest.fail(f"Firmware binary not found: {bin_path}")

    exporter_cfg = Path(__file__).parent / "exporter-ai-legion-nfc.yaml"
    try:
        stlink_serial = stm32_flash.resolve_stlink_serial(exporter_cfg)
    except ValueError as e:
        pytest.fail(str(e))

    # Canonical shared-board flash path (stm32_flash module docstring):
    # --connect-under-reset write + reset, ST-LINK USBDEVFS_RESET + one
    # retry on latch. A plain `--reset write` wedges the target's USB
    # under foreign firmware (bolty bench-sharing lesson) and cannot
    # attach at all when running USB-CDC firmware has locked out SWD
    # (micronuts lesson) — the two recurring F469 wedge classes.
    ok, log = stm32_flash.flash_f469(str(bin_path), stlink_serial)
    if not ok:
        pytest.fail(f"st-flash failed (recovery attempted, see log):\n{log}")
    time.sleep(USB_RESCAN_DELAY_S)
    yield str(bin_path)


@pytest.fixture(scope="session")
def cherry_reader(helpers, flashed_firmware):
    import bench as benchmod

    absent = benchmod.dut_absent_reason("stm32-ccid")
    if absent:
        pytest.skip(f"stm32-ccid: {absent}")
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
