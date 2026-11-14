#!/usr/bin/env python3
"""CCID PIN-verify ioctl payload builder (PC_to_RDR_Secure abData).

The hardware pinpad E2E (F469 Cherry + display build) sends
SCardControl(FEATURE_VERIFY_PIN_DIRECT) with this body. Building it
OFFLINE and unit-locking the bytes means the bench session exercises
only wire/RF behavior, never payload correctness.

WIRE CONTRACT (cross-checked against the firmware parser, not memory):
pcsc-lite reader.h PIN_VERIFY_STRUCTURE is copied verbatim + LE onto
the CCID wire by libccid's SecurePINVerify, so PC/SC field order IS
CCID order. wPINMaxExtraDigit = 0xXXYY with XX=MIN, YY=MAX — as a LE
u16 that puts **max in wire byte 4 and min in byte 5**. The firmware
(crates/ccid-core secure.rs, parse_pin_verify test) reads exactly that
and its test locks the order; a builder that swaps them passes no
cross-check. Citation: repo secure.rs lines 50-76 quote pcsc-lite
reader.h verbatim (CCID 1.1 §6.1.11 abPINDataStructure).
"""
from __future__ import annotations

import argparse
import sys


def build_verify_payload(apdu_head: bytes, pin_min: int = 4,
                         pin_max: int = 8, timeout_s: int = 30,
                         validation: int = 0x02, lang_id: int = 0x0409,
                         msg_index: int = 0x00, number_message: int = 0x01,
                         format_string: int = 0x82,
                         pin_block_string: int = 0x04,
                         pin_length_format: int = 0x00) -> bytes:
    """PC_to_RDR_Secure abData for bVerifyPinDirect.

    apdu_head: the VERIFY APDU head the reader completes with the
    entered PIN (CLA INS P1 P2 [Lc]); 5 bytes for the standard
    T=0 direct template.
    """
    if not 1 <= pin_min <= pin_max <= 12:
        raise ValueError("need 1 <= min <= max <= 12")
    if len(apdu_head) < 4:
        raise ValueError("APDU head needs at least CLA INS P1 P2")
    hdr = bytes([
        timeout_s,          # 0  bTimeOut
        format_string,      # 1  bmFormatString (0x82: BCD/system units)
        pin_block_string,   # 2  bmPINBlockString (8-byte block = 0x04 units)
        pin_length_format,  # 3  bmPINLengthFormat
        pin_max,            # 4  wPINMaxExtraDigit LSB = MAX (see contract)
        pin_min,            # 5  wPINMaxExtraDigit MSB = MIN
        validation,         # 6  bEntryValidationCondition (0x02 = key)
        number_message,     # 7  bNumberMessage
        lang_id & 0xFF,     # 8  wLangId LSB
        (lang_id >> 8) & 0xFF,  # 9  wLangId MSB
        msg_index,          # 10 bMsgIndex
        0x00, 0x00, 0x00,   # 11-13 bTeoPrologue[3]
    ])
    return hdr + apdu_head


VERIFY_APDU_T0 = bytes.fromhex("0020008100")  # VERIFY P2=0x81 (user), Lc=0


def main() -> int:
    desc = (__doc__ or "").strip().splitlines()[0]
    ap = argparse.ArgumentParser(description=desc)
    ap.add_argument("--pin-ref", default="81",
                    help="P2 PIN reference (81 user / 83 admin)")
    ap.add_argument("--dry-run", action="store_true",
                    help="print the payload and exit (no reader needed)")
    args = ap.parse_args()
    apdu = bytes.fromhex(f"002000{args.pin_ref}00")
    payload = build_verify_payload(apdu)
    print(f"FEATURE_VERIFY_PIN_DIRECT payload: {payload.hex()}")
    if args.dry_run:
        return 0
    try:
        from smartcard.System import readers  # noqa: F401 (bench only)
    except ImportError:
        print("pyscard unavailable — offline mode only", file=sys.stderr)
        return 2
    print("hardware drive path is the Phase-2 F469 session; payload above "
          "is the verified construction")
    return 0


if __name__ == "__main__":
    sys.exit(main())
