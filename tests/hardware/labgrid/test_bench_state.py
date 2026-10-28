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
    of a bench we still own. Absent hardware / swapped firmware variants
    SKIP with the detection reason (SKIP≠FAIL, #183)."""
    import bench as benchmod

    absent = benchmod.dut_absent_reason(place_name)
    if absent:
        pytest.skip(f"{place_name}: {absent}")
    probe = IDENTITY_PROBES[place_name]
    if place_name in SERIAL_PROBES:
        # Broker/CDC ownership (nucula): the broker is the board's
        # sanctioned serving mode and may hold the CDC when this runs —
        # a raw GemPC probe under it is the documented wedge class
        # (2026-10-14 nightly, twice). The fast path inside probe_nucula
        # cannot fire under pcscd_paused() (pcscd is DOWN when it lists
        # readers), so ask BEFORE pausing; a serving broker IS the
        # identity evidence (its live reader name), same as the battery
        # suite's guard.
        if place_name == "nucula-c3":
            from board_identity import nucula_broker_serving

            serving = nucula_broker_serving()
            if serving:
                result = {"ok": True,
                          "detail": f"serving pcscd via broker ({serving[0][:40]})"}
            else:
                with pcscd_paused():
                    result = probe()
        else:
            with pcscd_paused():
                result = probe()
    else:
        _pcscd(stop=False)
        result = probe()
    assert result["ok"], f"{place_name}: {result['detail']}"


def selected_duts(places_csv: str) -> dict[str, str]:
    """#334: the pcscd uniqueness check covers only the operator's
    --places selection. An unselected-yet-present wedged DUT must not
    fail a run the operator explicitly scoped away from it."""
    wanted = {p.strip() for p in places_csv.split(",") if p.strip()}
    return {p: i for p, i in DUT_IDENTITIES.items() if p in wanted}


def test_dut_identities_unique_in_pcscd(request, bench_lock):
    """Doctrine rule 3: each DUT identity matches EXACTLY ONE pcscd
    reader — the bench's reference devices (ACR1252, CardMan, NR7101)
    can never collide with our emulated identities. Absent hardware
    contributes no assertion (#183 SKIP≠FAIL); unselected places are
    out of scope (#334 — the check respects --places)."""
    import bench as benchmod

    duts = selected_duts(request.config.getoption("--places"))
    if not duts:
        pytest.skip("--places selected no DUT identities to check")

    _pcscd(stop=False)
    absent = []
    for place, identity in duts.items():
        holder = benchmod.place_holder(place)
        if holder:
            pytest.skip(f"{place} held by {holder} — its pcscd state is theirs")
        reason = benchmod.dut_absent_reason(place)
        if reason:
            absent.append(f"{place}: {reason}")
    checkable = [(p, i) for p, i in duts.items()
                 if not any(p in a for a in absent)]
    if not checkable:
        pytest.skip("no DUT hardware testable — " + "; ".join(absent))
    for place, identity in checkable:
        matches = pcsc_reader_matches(identity)
        assert matches, f"DUT identity missing from pcscd: {identity}"
        assert len(matches) == 1, (
            f"DUT identity {identity!r} ambiguous ({len(matches)} readers): "
            f"{matches}"
        )
