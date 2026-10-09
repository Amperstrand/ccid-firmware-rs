"""Serial CCID battery as a HIL test (#65): per serial DUT place, run
the conformance subset + fuzz resync proofs + a short soak. pcscd stays
paused for the duration (the GemPC wire is exclusive to the probe).
"""

import sys
from pathlib import Path

import pytest

from bench import ledger

pytestmark = pytest.mark.hil

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "nfc"))
from conformance_battery import (  # noqa: E402
    MSG_ESCAPE, MSG_GET_PARAMETERS, MSG_GET_SLOT_STATUS, MSG_ICC_POWER_OFF,
    MSG_ICC_POWER_ON, MSG_XFR_BLOCK, Reader, frame,
)

from test_bench_state import PLACE_TO_PORT, pcscd_paused  # noqa: E402

FUZZ_ROUNDS = 10
SOAK_ROUNDS = 50


def _conformance_cases():
    # response frame shape: (type, seq, bStatus, bError, payload)
    return [
        ("escape-version", MSG_ESCAPE, b"\x02",
         lambda r: r and r[0][0] == 0x83 and len(r[0][4]) > 0),
        ("escape-sync", MSG_ESCAPE, b"\x01\x01\x01",
         lambda r: r and r[0][0] == 0x83),
        ("slot-status", MSG_GET_SLOT_STATUS, b"",
         lambda r: r and r[0][0] == 0x81),
        ("power-on", MSG_ICC_POWER_ON, b"",
         lambda r: bool(r)),
        ("power-off", MSG_ICC_POWER_OFF, b"",
         lambda r: bool(r)),
        ("set-parameters", MSG_GET_PARAMETERS, b"",
         lambda r: bool(r)),
        ("xfr-block", MSG_XFR_BLOCK, b"\x00\xA4\x04\x00\x00",
         # card-present: DataBlock relay (any bStatus); card-absent:
         # failed DataBlock/SlotStatus — both prove the serve path
         lambda r: r and r[0][0] in (0x80, 0x81)),
    ]


def _run_cases(reader: Reader) -> list[str]:
    failures = []
    for name, mt, data, expect in _conformance_cases():
        resp = reader.exchange(mt, data)
        if not expect(resp):
            failures.append(name)
    return failures


def _run_fuzz(reader: Reader) -> list[str]:
    import random
    rng = random.Random(20261009)  # same seed as the full battery
    cases = [
        ("bad-lrc", frame(MSG_GET_SLOT_STATUS, 1, lrc_override=0xFF)),
        ("truncated-header", bytes([0x03, 0x06, 0x65, 0x01])),
        ("no-sync-garbage", bytes(rng.randbytes(24))),
        ("oversized-dwlength",
         bytes([0x03, 0x06, 0x6F, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x01, 0, 0, 0])
         + bytes([0x00])),
        ("lone-sync", bytes([0x03])),
        ("nak-ctrl", bytes([0x03, 0x15])),
    ]
    for rnd in range(FUZZ_ROUNDS):
        body = bytes(rng.randbytes(rng.randint(1, 48)))
        if rng.random() < 0.7:
            body = bytes([0x03]) + body
        cases.append((f"random-{rnd}", body))

    wedges = []
    for name, raw in cases:
        reader.s.write(raw)
        resp = reader.exchange(MSG_GET_SLOT_STATUS, settle=0.6)
        if not (resp and resp[0][0] == 0x81):
            # Recovery proof (the full battery's #96 pattern): a slow
            # answer is a slow path, only a probe that stays dead is a
            # wedge. The m5stick's truncated-header case reads ~0.65 s
            # recovery on BOTH readers — measurement floor, not firmware.
            recovered = any(
                (r := reader.exchange(MSG_GET_SLOT_STATUS, settle=0.4))
                and r[0][0] == 0x81
                for _ in range(3)
            )
            if not recovered:
                wedges.append(name)
    return wedges


def _run_soak(reader: Reader) -> tuple[int, int]:
    clean = 0
    first_icc = None
    for _ in range(SOAK_ROUNDS):
        resp = reader.exchange(MSG_GET_SLOT_STATUS, settle=0.01)
        if resp and resp[0][0] == 0x81:
            icc = resp[0][2] & 7
            if first_icc is None:
                first_icc = icc
            if icc == first_icc:
                clean += 1
    return clean, SOAK_ROUNDS


@pytest.mark.parametrize("place_name", ["m5stick", "nucula-c3"], indirect=True)
def test_serial_dut_battery(place_name, bench_lock):
    """The battery's core, one DUT at a time, place held."""
    port_name = PLACE_TO_PORT[place_name]
    from serial_ports import resolve

    with pcscd_paused():
        reader = Reader(port_name, resolve(port_name))
        failures = _run_cases(reader)
        assert not failures, f"{place_name}: conformance failures: {failures}"

        wedges = _run_fuzz(reader)
        reader.close()

        clean, total = _run_soak(Reader(port_name, resolve(port_name)))

    ledger(
        "serial_battery",
        place=place_name,
        wedges=wedges,
        soak=f"{clean}/{total}",
    )
    assert not wedges, f"{place_name}: fuzz wedges: {wedges}"
    assert clean == total, f"{place_name}: soak {clean}/{total} (icc flapped)"
