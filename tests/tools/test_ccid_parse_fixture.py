"""CI host test: our CCID class descriptors vs libccid's parse.c checks (#143).

Fixtures are generated from the Rust device-profile consts (single source
of truth) by ``firmware/ccid-firmware/src/device_profile.rs::
descriptor_fixtures_match_profile_consts`` — freshness is locked by the
Rust suite; this suite runs the ported parse.c checker (see
``ccid_parse_fixture.py`` for the port rules) against those bytes.

Deterministic, pure stdlib+pytest — belongs in the python-collect minimal
env (runs via ``python3 -m pytest tests/tools/ -q``), never needs hardware.
"""

from pathlib import Path
from sys import path as sys_path

import pytest

HERE = Path(__file__).resolve().parent
sys_path.insert(0, str(HERE))
import ccid_parse_fixture as cpf  # noqa: E402

# Reference reader specs (authoritative per AGENTS.md reference-first
# doctrine): reference/CCID/readers/*.txt. Only the values those files
# actually pin are asserted here — everything else is checked structurally
# by the parse.c port.
EXPECTED = {
    "cherry-smartterminal-st2xxx": {
        # CherrySmartTerminalST2XXX.txt: dwFeatures 0x000100BA (TPDU level),
        # bPINSupport 0x03 (verify+modify), 5V only, T=0|T=1.
        "dw_features": 0x000100BA,
        "b_pin_support": 0x03,
        "b_voltage_support": 0x01,
        "dw_protocols": 0x03,
        "dw_max_ccid_message_length": 270,
    },
    "gemalto-idbridge-ct30": {
        # Gemalto_IDBridge_CT30.txt: dwFeatures 0x00010230 (TPDU level),
        # bPINSupport 0x00, wLcdLayout 0x0000, class bytes 0x00.
        "dw_features": 0x00010230,
        "b_pin_support": 0x00,
        "w_lcd_layout": 0x0000,
        "b_class_get_response": 0x00,
        "b_class_envelope": 0x00,
    },
    "gemalto-idbridge-k30": {
        # Gemalto_IDBridge_K30.txt: virtually identical to CT30
        # (PID 3438). The K30 PIN-pad incident (falsely claimed
        # bPINSupport/LCD) is exactly what these asserts lock out.
        "dw_features": 0x00010230,
        "b_pin_support": 0x00,
        "w_lcd_layout": 0x0000,
        "b_class_get_response": 0x00,
        "b_class_envelope": 0x00,
    },
}


@pytest.mark.parametrize("name", sorted(cpf.FIXTURE_PROFILES))
def test_fixture_passes_parse_c_checks(name):
    data = cpf.load_fixture(name)
    report = cpf.check_ccid_class_descriptor(data)
    assert report.errors == [], f"{name}: {report.errors}"


@pytest.mark.parametrize("name", sorted(cpf.FIXTURE_PROFILES))
def test_fixture_header_is_ccid_class_descriptor(name):
    data = cpf.load_fixture(name)
    assert len(data) == 0x36
    assert data[0] == 0x36  # parse.c:418
    assert data[1] == 0x21  # parse.c:425


@pytest.mark.parametrize("name", sorted(EXPECTED))
def test_reference_pinned_values(name):
    report = cpf.check_ccid_class_descriptor(cpf.load_fixture(name))
    for attr, want in EXPECTED[name].items():
        got = getattr(report, attr)
        assert got == want, f"{name}.{attr}: {got:#x} != reference {want:#x}"


@pytest.mark.parametrize("name", sorted(cpf.FIXTURE_PROFILES))
def test_exchange_level_is_tpdu(name):
    # All three shipping profiles are TPDU level (the #132-era descriptor
    # correction; README's "Short APDU level" line is stale).
    report = cpf.check_ccid_class_descriptor(cpf.load_fixture(name))
    assert (report.dw_features >> 16) & 0x07 == 0x01
    assert any("TPDU level exchange" in n for n in report.notes)


def test_cherry_advertises_auto_pps_but_not_auto_ifsd():
    # The two bits the t1_relay/#132 analysis leans on (docs/
    # t1_relay_132_rootcause.md A.7): AUTO_PPS_CUR set => stock libccid
    # never generates PPS; AUTO_IFSD clear => host runs t1_negotiate_ifsd.
    r = cpf.check_ccid_class_descriptor(cpf.load_fixture("cherry-smartterminal-st2xxx"))
    assert r.dw_features & 0x80  # AUTO_PPS_CUR
    assert not r.dw_features & 0x0400  # AUTO_IFSD


# --- negative cases: the checker must flag what parse.c flags ---


def test_bad_blength_is_error():
    data = bytearray(cpf.load_fixture("cherry-smartterminal-st2xxx"))
    data[0] = 0x35
    report = cpf.check_ccid_class_descriptor(bytes(data))
    assert any("bLength" in e for e in report.errors)


def test_bad_bdescriptor_type_is_error():
    data = bytearray(cpf.load_fixture("cherry-smartterminal-st2xxx"))
    data[1] = 0x22
    report = cpf.check_ccid_class_descriptor(bytes(data))
    assert any("bDescriptorType" in e for e in report.errors)


def test_wrong_length_is_error():
    report = cpf.check_ccid_class_descriptor(b"\x36\x21" + b"\x00" * 40)
    assert report.errors and "54" in report.errors[0]


def test_invalid_exchange_level_is_error():
    # bits 16..18 = 0x03: none of parse.c's switch arms — the port's
    # documented upgrade of parse.c's silent fall-through.
    data = bytearray(cpf.load_fixture("cherry-smartterminal-st2xxx"))
    data[42] = (data[42] & ~0x07) | 0x03
    report = cpf.check_ccid_class_descriptor(bytes(data))
    assert any("exchange level" in e for e in report.errors)


def test_k30_pin_pad_regression_locked():
    # The K30 once falsely claimed a PIN pad + LCD; the fixture must
    # never grow those bits back.
    r = cpf.check_ccid_class_descriptor(cpf.load_fixture("gemalto-idbridge-k30"))
    assert r.b_pin_support == 0
    assert r.w_lcd_layout == 0
