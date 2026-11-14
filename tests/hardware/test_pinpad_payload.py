#!/usr/bin/env python3
"""Offline unit tests for the CCID PIN-verify payload builder.

Locks the wPINMaxExtraDigit byte order (max LSB / min MSB — the
pcsc-lite reader.h 0xXXYY, XX=min YY=max contract quoted in the
firmware's secure.rs citation) against accidental swap, plus the
concrete byte image the hardware run will send. Pure offline: no
pyscard, no reader, safe for the CI collect gate.
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from tests.hardware.pinpad_payload import (  # noqa: E402
    VERIFY_APDU_T0, build_verify_payload,
)


def test_wire_byte_order_max_lsb_min_msb():
    p = build_verify_payload(VERIFY_APDU_T0, pin_min=4, pin_max=8)
    # pcsc-lite: wPINMaxExtraDigit = 0x0408 (XX=min=04, YY=max=08) as LE
    # u16 -> byte 4 = 08 (max), byte 5 = 04 (min). A swap here inverts
    # the length bounds the firmware pinpad UI enforces.
    assert p[4] == 0x08 and p[5] == 0x04


def test_full_byte_image():
    p = build_verify_payload(VERIFY_APDU_T0, pin_min=4, pin_max=8,
                             timeout_s=30)
    expected = bytes.fromhex(
        "1e"      # bTimeOut 30
        "82"      # bmFormatString
        "04"      # bmPINBlockString
        "00"      # bmPINLengthFormat
        "08" "04"  # wPINMaxExtraDigit LE: max, min
        "02"      # validation: key press
        "01"      # one message
        "0904"    # wLangId 0x0409 LE
        "00"      # bMsgIndex
        "000000"  # bTeoPrologue
    ) + VERIFY_APDU_T0
    assert p == expected


def test_header_is_14_bytes_then_apdu():
    p = build_verify_payload(b"\x00\x20\x00\x83")
    assert len(p) == 14 + 4
    assert p[14:] == b"\x00\x20\x00\x83"  # admin PIN ref template


def test_bounds_rejected():
    for bad in ({"pin_min": 9, "pin_max": 8}, {"pin_min": 0, "pin_max": 8},
                {"pin_min": 4, "pin_max": 13}):
        try:
            build_verify_payload(VERIFY_APDU_T0, **bad)
        except ValueError:
            continue
        raise AssertionError(f"bounds {bad} should reject")


def test_short_apdu_rejected():
    try:
        build_verify_payload(b"\x00\x20\x00")
    except ValueError:
        return
    raise AssertionError("3-byte APDU head should reject")
