"""
HIL tests for ccid-firmware-rs on STM32F469I-DISCO.

Run:
  pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil \
    --firmware-bin=target/thumbv7em-none-eabihf/release/ccid-firmware.bin

Tests assume the ComSign eID T=1 card is in the slot (ATR: 3B D5 18 FF ...).
"""

import pytest

from conftest import EXPECTED_ATR, remote_apdu

try:
    from smartcard.pcsc.PCSCPart10 import (
        getFeatureRequest, hasFeature, FEATURE_CCID_ESC_COMMAND, SCARD_CTL_CODE
    )
    from smartcard.scard import SCARD_SHARE_DIRECT, SCARD_LEAVE_CARD
    _HAS_PSCARD = True
except ImportError:
    _HAS_PSCARD = False

pytestmark = pytest.mark.hil


def test_usb_enumeration_cherry(cherry_reader):
    result = cherry_reader.run("lsusb")
    assert "046a:003e" in result.stdout
    assert "CHERRY" in result.stdout.upper()


def test_pcscd_detects_reader(cherry_reader, pcscd_running):
    result = pcscd_running.run("timeout 4 pcsc_scan 2>&1 | head -15")
    assert "Cherry" in result.stdout or "ST-2xxx" in result.stdout
    assert "Card state: Card inserted" in result.stdout


def test_card_atr_matches_expected(pcsc_reader_name, cherry_reader):
    result = cherry_reader.run("python3 /tmp/hil_atr.py", timeout=10)
    atr = result.stdout.strip()
    assert atr == EXPECTED_ATR, (
        f"ATR mismatch:\n  expected: {EXPECTED_ATR}\n  got:      {atr}"
    )


def test_apdu_select_mf_returns_sw(pcsc_reader_name, cherry_reader):
    _, sw1, sw2 = remote_apdu(cherry_reader, "00A40000")
    assert sw1 in (0x6A, 0x90, 0x6E), f"Unexpected SW: {sw1:02X} {sw2:02X}"


def test_apdu_get_challenge_returns_class_not_supported(pcsc_reader_name, cherry_reader):
    _, sw1, sw2 = remote_apdu(cherry_reader, "0084000008")
    assert sw1 == 0x6E, f"Expected 6E 00 (CLASS not supported), got {sw1:02X} {sw2:02X}"


def test_reader_advertises_pinpad(cherry_reader, pcscd_running):
    result = pcscd_running.run("timeout 4 pcsc_scan 2>&1 | head -15")
    lines = result.stdout.split("\n")
    reader_line = next((l for l in lines if "Cherry" in l or "ST-2xxx" in l), "")
    assert reader_line, "Reader line not found in pcsc_scan output"

@pytest.mark.skipif(not _HAS_PSCARD, reason="pyscard not installed")
def test_escape_diagnostic_returns_counters(cherry_reader, pcscd_running):
    """Send CCID Escape [0xD0] and verify 28-byte diagnostic response."""
    import struct
    from smartcard.System import readers

    r = readers()[0]
    conn = r.createConnection()
    conn.connect(mode=SCARD_SHARE_DIRECT, disposition=SCARD_LEAVE_CARD)

    try:
        features = getFeatureRequest(conn)
        esc_ioctl = hasFeature(features, FEATURE_CCID_ESC_COMMAND)
        if esc_ioctl is None:
            esc_ioctl = SCARD_CTL_CODE(1)
        resp = conn.control(esc_ioctl, [0xD0])
        assert len(resp) == 28, f"Expected 28 bytes, got {len(resp)}"
        diag = bytes(resp)
        fields = ['apdu_tx', 'apdu_rx', 'nak', 'error', 'reinit', 'card_present', 'uptime']
        for i, name in enumerate(fields):
            val = struct.unpack_from('<I', diag, i * 4)[0]
            print(f"  {name}: {val}")
        card_present = struct.unpack_from('<I', diag, 20)[0]
        assert card_present in (0, 1), f"card_present should be 0 or 1, got {card_present}"
    finally:
        conn.disconnect()
