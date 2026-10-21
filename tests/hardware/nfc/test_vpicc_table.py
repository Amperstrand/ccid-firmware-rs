"""Bench/CI test: the scripted vpcd table card (#261 tier a).

Requires a pcscd vpcd reader slot pointed at the mock's port. Bench
setup (one-time):

    printf 'FRIENDLYNAME "vPICC table card"\\nDEVICENAME /dev/null:0x8C82\\nLIBPATH /usr/local/lib/pcsc/drivers/serial/libifdvpcd-amperstrand.so\\nCHANNELID 0x8C82\\n' \\
      > /etc/reader.conf.d/vpicc-table
    systemctl restart pcscd.socket pcscd   # port 35970 = 0x8C82

Then: python3 -m pytest tests/hardware/nfc/test_vpicc_table.py -v
"""

import os
import time

import pytest

from vpicc_table import MARKER, DEFAULT_ATR, TableVpicc

pyscard = pytest.importorskip("smartcard")  # bench/CI gate
from smartcard.System import readers  # noqa: E402

# Slot binding: the PATCHED libifdvpcd (0x8C82 base) re-polls absent
# readers properly (#359 fix) — the daemon may attach any time, unlike
# the stock driver whose first absent probe kills the EH thread.
PORT = 35970
SLOT = "00 00"
READER_MARK = "vPICC table card"
POLL_S = 20.0


def _reader():
    for r in readers():
        if READER_MARK in str(r) and str(r).endswith(SLOT):
            return r
    return None


def _card_present() -> bool:
    r = _reader()
    if r is None:
        return False
    try:
        conn = r.createConnection()
        conn.connect()
        conn.disconnect()
        return True
    except Exception:
        return False


@pytest.fixture(scope="module")
def card():
    # The patched driver re-polls absent readers (#359 fix), so the
    # daemon may attach at any time. Poll until a connect SUCCEEDS —
    # one success is proof; re-checking immediately after can race the
    # probe cadence and flap.
    c = TableVpicc("127.0.0.1", PORT, DEFAULT_ATR)
    c.start()
    # Readiness WITHOUT touching pcscd: a pyscard connect/disconnect
    # power-cycles the mock and leaves the slot flapping — instead wait
    # until the daemon has ANSWERED one of the driver's own probes.
    deadline = time.time() + POLL_S
    assert c.answered_probe.wait(POLL_S), (
        "driver never probed the mock — check the daemon reaches port "
        f"{PORT} and the reader.conf entry (dedicated .so copy + "
        "patched driver; see module docstring)"
    )
    time.sleep(1.0)  # let pcscd register the presence transition
    yield c
    c.stop()


def _transmit(card, apdu: bytes, tries: int = 10):
    # pcscd presence transitions after a disconnect are asynchronous —
    # the slot may read NoCard for a moment before the driver's re-poll
    # registers the mock again. Bounded retries, not a sleep.
    last = None
    for _ in range(tries):
        r = _reader()
        assert r is not None, (
            f"pcscd never listed '{READER_MARK}' — add the reader.conf.d "
            "entry (see module docstring)"
        )
        try:
            conn = r.createConnection()
            conn.connect()
        except Exception as e:
            last = e
            time.sleep(0.5)
            continue
        try:
            data, sw1, sw2 = conn.transmit(list(apdu))
            return bytes(data), (sw1 << 8) | sw2
        finally:
            conn.disconnect()
    raise AssertionError(f"card never re-present after retries: {last}")


def test_atr_shape(card):
    for _ in range(10):
        r = _reader()
        assert r is not None, "reader slot missing (see docstring)"
        try:
            conn = r.createConnection()
            conn.connect()
        except Exception:
            time.sleep(0.5)
            continue
        try:
            assert bytes(conn.getATR()) == bytes.fromhex(DEFAULT_ATR)
            return
        finally:
            conn.disconnect()
    raise AssertionError("ATR read never succeeded")


def test_select_isd_self_marked(card):
    apdu = bytes.fromhex("00A40400") + bytes([7]) + \
        bytes.fromhex("A0000001510000")
    data, sw = _transmit(card, apdu)
    assert sw == 0x9000
    assert MARKER in data, "every table answer must self-mark (#261)"


def test_cplc_is_synthetic(card):
    data, sw = _transmit(card, bytes.fromhex("80CA9F7F00"))
    assert sw == 0x9000
    assert data[:3] == bytes.fromhex("9F7F2D")
    assert set(data[3:]) <= {0xA5, 0x5A}, "CPLC filler must be synthetic"


def test_unknown_aid_rejected(card):
    apdu = bytes.fromhex("00A40400") + bytes([4]) + \
        bytes.fromhex("A000000291")
    _, sw = _transmit(card, apdu)
    assert sw == 0x6A82
