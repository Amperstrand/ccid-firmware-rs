"""Bench known-good state tests (#65): per-place identity probes — the
AGENTS.md matrix as pytest. Each test holds its place; busy places skip
with the holder named. Serial probes pause pcscd for the probe duration
(the GemPC wire is exclusive) and always restart it.
"""

import contextlib
import subprocess
import time

import pytest

from board_identity import IDENTITY_PROBES, pcsc_reader_matches

pytestmark = pytest.mark.hil

SERIAL_PROBES = ("m5stick", "nucula-c3")

# labgrid place name → serial_ports.py resolver name
PLACE_TO_PORT = {"m5stick": "m5stick", "nucula-c3": "nucula"}

# DUT identity → its labgrid place (a held place means another session
# owns that board's state — its pcscd presence is not ours to assert)
DUT_IDENTITIES = {
    "stm32-ccid": "ST2XXX-001",
    "m5stick": "GemPCTwin",
    "nucula-c3": "Nucula CCID",
}


def _pcscd(stop: bool) -> None:
    action = "stop" if stop else "start"
    subprocess.run(["systemctl", action, "pcscd.socket", "pcscd.service"],
                   capture_output=True)
    if stop:
        time.sleep(1.0)
        return
    # start: poll for readiness — rapid toggles need longer than a
    # fixed sleep (bench-observed flake)
    for _ in range(20):
        r = subprocess.run(["systemctl", "is-active", "pcscd.service"],
                           capture_output=True, text=True)
        if r.stdout.strip() == "active":
            time.sleep(0.5)
            return
        time.sleep(0.5)


@contextlib.contextmanager
def pcscd_paused():
    _pcscd(stop=True)
    try:
        yield
    finally:
        _pcscd(stop=False)


def test_place_identity(place_name, bench_lock):
    """Every selected place answers its identity probe — the definition
    of a bench we still own."""
    probe = IDENTITY_PROBES[place_name]
    if place_name in SERIAL_PROBES:
        with pcscd_paused():
            result = probe()
    else:
        _pcscd(stop=False)
        result = probe()
    assert result["ok"], f"{place_name}: {result['detail']}"


def test_dut_identities_unique_in_pcscd(bench_lock):
    """Doctrine rule 3: each DUT identity matches EXACTLY ONE pcscd
    reader — the bench's reference devices (ACR1252, CardMan, NR7101)
    can never collide with our emulated identities."""
    import bench as benchmod

    _pcscd(stop=False)
    for place, identity in DUT_IDENTITIES.items():
        holder = benchmod.place_holder(place)
        if holder:
            pytest.skip(f"{place} held by {holder} — its pcscd state is theirs")
        matches = pcsc_reader_matches(identity)
        assert matches, f"DUT identity missing from pcscd: {identity}"
        assert len(matches) == 1, (
            f"DUT identity {identity!r} ambiguous ({len(matches)} readers): "
            f"{matches}"
        )
