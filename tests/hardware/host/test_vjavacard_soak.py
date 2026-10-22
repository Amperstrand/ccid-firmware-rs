"""Virtual-stack soak: connect/SELECT/read cycling against vjavacard.

Mirrors the bench soak doctrine (m5stick serial 1000/1000): the virtual
reader stack (pcscd → patched libifdvpcd → jcardsim bridge) gets its own
reliability number. Zero-risk APDU set only (SELECT + pure reads/echoes
of caller bytes — the card-safety table), which on the virtual card
additionally makes the soak free of any state concerns.

Mode-aware: SmartPGP mode (bench, real applet, GET DATA reads) when its
AID selects; otherwise echo mode (CI runner — the SmartPGP CAP artifact
is bench storage). Both exercise the identical stack under soak.

Bench-side (SmartPGP bridge on 0x8C85):
    python3 -m pytest tests/hardware/host/test_vjavacard_soak.py -v -s

Soak length via VJC_SOAK_N (default 300; CI uses 100).
"""

import os
import time

import pytest

from test_vjavacard import ECHO_AID, SMARTPGP_AID, _bridge_ready, _connect

# Collection-safe import (#403 follow-up pattern — the soak file rode in
# with #416 without it and broke the python-collect gate's minimal env
# (pytest + stdlib, no pyscard) on every lint-filtered run; the tests
# themselves only ever run on the bench host where pyscard is installed).
try:
    from smartcard.Exceptions import (
        CardConnectionException,
        ListReadersException,
        NoCardException,
    )

    _HAS_SMARTCARD = True
except ImportError:  # pragma: no cover — CI runners
    _HAS_SMARTCARD = False
    CardConnectionException = ListReadersException = NoCardException = None

pytestmark = [
    pytest.mark.skipif(
        not _HAS_SMARTCARD,
        reason="pyscard not installed — vjavacard soak drives pcscd (bench only)",
    ),
    pytest.mark.skipif(
        not _bridge_ready(), reason="vjavacard bridge not built/running (host setup)"
    ),
]

SOAK_N = int(os.environ.get("VJC_SOAK_N", "300"))


def _mode():
    """Return (mode, aid) — SmartPGP when its AID selects, else echo."""
    for mode, aid_hex in (("smartpgp", SMARTPGP_AID), ("echo", ECHO_AID)):
        try:
            conn = _connect()
            try:
                aid = bytes.fromhex(aid_hex)
                _, s1, s2 = conn.transmit(
                    [0x00, 0xA4, 0x04, 0x00, len(aid)] + list(aid)
                )
                if (s1, s2) == (0x90, 0x00):
                    return mode, aid_hex
            finally:
                conn.disconnect()
        except Exception:
            continue
    pytest.skip("no applet mode available (bridge absent?)")


def test_connect_select_read_soak():
    """N full cycles: connect → SELECT → pure read → disconnect.

    Prints the pass rate and per-failure diagnostics; asserts every cycle
    passed (an honest soak number, same as the bench doctrine — flakes
    get investigated, not budgeted).
    """
    mode, aid_hex = _mode()
    aid = bytes.fromhex(aid_hex)
    print(f"\nSOAK mode: {mode} (aid {aid_hex})")
    ok = 0
    failures = []
    t0 = time.monotonic()
    for i in range(SOAK_N):
        try:
            conn = _connect()
            try:
                _, s1, s2 = conn.transmit(
                    [0x00, 0xA4, 0x04, 0x00, len(aid)] + list(aid)
                )
                assert (s1, s2) == (0x90, 0x00), f"cycle {i}: SELECT {s1:02x}{s2:02x}"
                if mode == "smartpgp":
                    data, s1, s2 = conn.transmit([0x00, 0xCA, 0x00, 0x4F])
                    assert (s1, s2) == (0x90, 0x00), (
                        f"cycle {i}: GET DATA {s1:02x}{s2:02x}"
                    )
                    assert bytes(data).hex().upper().startswith("D27600012401")
                else:
                    payload = [0xBA, 0xAD]
                    data, s1, s2 = conn.transmit(
                        [0x80, 0xAA, 0x00, 0x00, 0x02] + payload
                    )
                    assert (s1, s2) == (0x90, 0x00), f"cycle {i}: ECHO {s1:02x}{s2:02x}"
                    assert list(data) == payload + [0x90, 0x00]
                ok += 1
            finally:
                conn.disconnect()
        except (
            AssertionError,
            CardConnectionException,
            NoCardException,
            ListReadersException,
        ) as e:
            failures.append(f"cycle {i}: {e}")
    dt = time.monotonic() - t0
    print(
        f"\nSOAK: {ok}/{SOAK_N} cycles green in {dt:.1f}s "
        f"({dt / max(ok, 1) * 1000:.0f} ms/cycle)"
    )
    if failures:
        print("failures:", *failures[:10], sep="\n  ")
    assert ok == SOAK_N, f"soak {ok}/{SOAK_N}: {failures[:5]}"
