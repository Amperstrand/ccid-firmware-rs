"""
HIL tests for ccid-firmware-rs on STM32F469I-DISCO.

Run:
  pytest tests/hardware/labgrid/test_ccid_hil.py -v --hil \
    --firmware-bin=target/thumbv7em-none-eabihf/release/ccid-firmware.bin

Tests assume the ComSign eID T=1 card is in the slot (ATR: 3B D5 18 FF ...).
"""

import re
import struct

import pytest

from conftest import EXPECTED_ATR, remote_apdu

pytestmark = pytest.mark.hil


def test_usb_enumeration_cherry(cherry_reader):
    result = cherry_reader.run("lsusb")
    assert "046a:003e" in result.stdout
    assert "CHERRY" in result.stdout.upper()


def test_pcscd_detects_reader(helpers, pcscd_running):
    """pcscd lists the DUT reader with the card present. The serial-selected
    fixture proves enumeration; this asserts the card is IN the slot."""
    result = helpers.run("python3 /tmp/hil-ccid-bench/hil_atr.py")
    out = result.stdout.strip()
    assert not out.startswith("ERROR:NOREADER"), f"DUT reader missing: {out}"
    assert out == EXPECTED_ATR, f"ATR mismatch: {out}"


def test_card_atr_matches_expected(pcsc_reader_name, cherry_reader):
    result = cherry_reader.run("python3 /tmp/hil-ccid-bench/hil_atr.py", timeout=10)
    atr = result.stdout.strip()
    assert atr == EXPECTED_ATR, (
        f"ATR mismatch:\n  expected: {EXPECTED_ATR}\n  got:      {atr}"
    )


def test_apdu_select_mf_returns_sw(pcsc_reader_name, cherry_reader):
    _, sw1, sw2 = remote_apdu(cherry_reader, "00A40000")
    assert sw1 in (0x6A, 0x90, 0x6E), f"Unexpected SW: {sw1:02X} {sw2:02X}"


def test_apdu_get_challenge_returns_class_not_supported(pcsc_reader_name, cherry_reader):
    _, sw1, sw2 = remote_apdu(cherry_reader, "0084000008")
    assert sw1 in (0x6D, 0x6E), f"Expected 6D/6E (unsupported), got {sw1:02X} {sw2:02X}"


def test_reader_advertises_pinpad(helpers, pcscd_running):
    """DUT advertises PIN support via the CCID feature list (the host-visible
    consequence of bPINSupport != 0; lsusb -v needs root, features don't)."""
    result = helpers.run(
        "python3 - <<'PYEOF'\n"
        "from smartcard.pcsc.PCSCPart10 import getFeatureRequest\n"
        "from smartcard.scard import SCARD_SHARE_DIRECT, SCARD_LEAVE_CARD\n"
        "from smartcard.System import readers\n"
        "rs = [r for r in readers() if 'ST2XXX-001' in str(r)]\n"
        "c = rs[0].createConnection()\n"
        "c.connect(mode=SCARD_SHARE_DIRECT, disposition=SCARD_LEAVE_CARD)\n"
        "feats = [f[0] for f in getFeatureRequest(c)]\n"
        "c.disconnect()\n"
        "print('VERIFY' if 'FEATURE_VERIFY_PIN_DIRECT' in feats else '-', end=' ')\n"
        "print('MODIFY' if 'FEATURE_MODIFY_PIN_DIRECT' in feats else '-')\n"
        "PYEOF"
    )
    out = result.stdout.strip()
    assert out == "VERIFY MODIFY", f"pinpad features missing: {out!r} ({result.stderr[:120]})"


def test_escape_diagnostic_returns_counters(cherry_reader, pcscd_running):
    """Escape [0xD0] via the REMOTE PC/SC stack.

    Codex review #47: the old version used the build host's local
    readers()[0] — it exercised whatever reader happened to be on the
    test machine (or nothing) instead of the HIL reader under test.
    """
    result = cherry_reader.run("python3 /tmp/hil-ccid-bench/hil_escape.py", timeout=10)
    assert result.returncode == 0, (
        f"remote escape helper failed:\nstdout: {result.stdout}\nstderr: {result.stderr}"
    )
    output = result.stdout.strip().split("\n")[-1]
    assert re.fullmatch(r"[0-9a-fA-F]+", output), (
        f"escape helper returned non-hex output: {output!r}"
    )
    diag = bytes.fromhex(output)
    assert len(diag) == 28, f"Expected 28 bytes, got {len(diag)}"
    fields = ['apdu_tx', 'apdu_rx', 'nak', 'error', 'reinit', 'card_present', 'uptime']
    for i, name in enumerate(fields):
        val = struct.unpack_from('<I', diag, i * 4)[0]
        print(f"  {name}: {val}")
    card_present = struct.unpack_from('<I', diag, 20)[0]
    assert card_present in (0, 1), f"card_present should be 0 or 1, got {card_present}"
