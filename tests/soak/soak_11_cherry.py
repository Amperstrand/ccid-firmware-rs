#!/usr/bin/env python3
"""Soak Test 11: Cherry ST-2xxx Profile Specific Tests

Tests CCID-level behavioral differences unique to the Cherry profile.
Since we don't have a real Cherry ST-2xxx reader for comparison, this suite:

1. Validates the firmware's USB descriptor matches the Cherry profile spec
   (self-consistency against device_profile.rs definitions)
2. Tests CCID behavior unique to Cherry (ESCAPE rejection)
3. Runs APDU cross-reader comparison against Gemalto (same JR3180 card,
   same APDU results expected regardless of reader profile)

Usage:
    sudo FIRMWARE_SERIAL=ST2XXX-001 python3 soak_11_cherry.py
"""

import subprocess
import sys
import os
import re
import time
import struct

sys.path.insert(0, os.path.dirname(__file__))
from soaklib import (
    discover_readers, connect_reader, disconnect_reader, restart_pcscd,
    stop_pcscd, transmit_apdu, run_test_on_both, save_results,
    SuiteResult, LOG_BASE, parse_ccid_log, find_escape_response,
    DEFAULT_FIRMWARE_SERIAL, GEMALTO_SERIAL, get_atr,
)

SUITE_DIR = LOG_BASE / "soak-11-cherry"
LOG_FILE = SUITE_DIR / "pcscd.log"

CHERRY_VID = 0x046A
CHERRY_PID = 0x003E
CHERRY_SERIAL = "ST2XXX-001"

# Expected values from device_profile.rs Cherry profile + BASE_PROFILE defaults.
# All offsets are relative to the CCID function descriptor start (after 2-byte USB header).
CHERRY_EXPECTED = {
    "bcdCCID": 0x0100,
    "bMaxSlotIndex": 0,
    "bVoltageSupport": 0x01,
    "dwProtocols": 0x00000003,
    "dwDefaultClock": 4000,
    "dwMaximumClock": 8000,
    "dwDataRate": 10753,
    "dwMaxDataRate": 344105,
    "dwMaxIFSD": 254,
    "dwSynchProtocols": 0,
    "dwMechanical": 0,
    # Firmware produces 0x000101BA (BASE_PROFILE includes FEAT_CLOCK_STOP=0x100).
    # Real Cherry ST-2xxx has 0x000100BA (no CLOCK_STOP).
    # This is a known difference — BASE_PROFILE features are inherited.
    "dwFeatures": 0x000101BA,
    "dwMaxCCIDMessageLength": 270,
    "bClassGetResponse": 0xFF,
    "bClassEnvelope": 0xFF,
    "wLcdLayout": 0x0000,
    "bPINSupport": 0x03,
    "bMaxCCIDBusySlots": 1,
}


def get_usb_descriptor() -> dict:
    result = subprocess.run(
        ["lsusb", "-d", f"{CHERRY_VID:04x}:{CHERRY_PID:04x}", "-v"],
        capture_output=True, text=True, timeout=10,
    )
    output = result.stdout
    info = {"vid": CHERRY_VID, "pid": CHERRY_PID}

    m = re.search(r"bInterfaceClass\s+(\d+)", output)
    if m:
        info["bInterfaceClass"] = int(m.group(1))

    m = re.search(r"bInterfaceSubClass\s+(\d+)", output)
    if m:
        info["bInterfaceSubClass"] = int(m.group(1))

    m = re.search(r"bInterfaceProtocol\s+(\d+)", output)
    if m:
        info["bInterfaceProtocol"] = int(m.group(1))

    m = re.search(r"bNumEndpoints\s+(\d+)", output)
    if m:
        info["bNumEndpoints"] = int(m.group(1))

    m = re.search(r"iManufacturer\s+\d+\s+(.*)", output)
    if m:
        info["iManufacturer"] = m.group(1).strip()

    m = re.search(r"iProduct\s+\d+\s+(.*)", output)
    if m:
        info["iProduct"] = m.group(1).strip()

    m = re.search(r"iSerial\s+\d+\s+(.*)", output)
    if m:
        info["iSerial"] = m.group(1).strip()

    raw_desc = re.search(r"\*\* UNRECOGNIZED:\s+([0-9A-Fa-f ]+)", output)
    if raw_desc:
        desc_bytes = bytes.fromhex(raw_desc.group(1).strip().replace(" ", ""))
        info["raw_hex"] = desc_bytes.hex()
        info["ccid"] = parse_ccid_function_descriptor(desc_bytes)

    return info


def parse_ccid_function_descriptor(data: bytes) -> dict:
    """Parse a 54-byte CCID function descriptor (bLength=54, bDescriptorType=0x21)."""
    if len(data) < 54:
        return {"error": f"Descriptor too short: {len(data)} bytes (need 54)"}

    b_len = data[0]
    b_type = data[1]
    if b_type != 0x21:
        return {"error": f"Not a CCID function descriptor (type=0x{b_type:02X})"}

    # All field offsets are relative to data[0] (the full USB descriptor)
    # CCID data starts at offset 2 (after bLength + bDescriptorType)
    ccid = {}
    ccid["bLength"] = b_len

    bcd = struct.unpack_from("<H", data, 2)[0]
    ccid["bcdCCID"] = f"{bcd >> 8}.{bcd & 0xFF:02d}"

    ccid["bMaxSlotIndex"] = data[4]
    ccid["bVoltageSupport"] = data[5]
    ccid["dwProtocols"] = struct.unpack_from("<I", data, 6)[0]
    ccid["dwDefaultClock"] = struct.unpack_from("<I", data, 10)[0]
    ccid["dwMaximumClock"] = struct.unpack_from("<I", data, 14)[0]
    ccid["bNumClockSupported"] = data[18]
    ccid["dwDataRate"] = struct.unpack_from("<I", data, 19)[0]
    ccid["dwMaxDataRate"] = struct.unpack_from("<I", data, 23)[0]
    ccid["bNumDataRatesSupported"] = data[27]
    ccid["dwMaxIFSD"] = struct.unpack_from("<I", data, 28)[0]
    ccid["dwSynchProtocols"] = struct.unpack_from("<I", data, 32)[0]
    ccid["dwMechanical"] = struct.unpack_from("<I", data, 36)[0]
    ccid["dwFeatures"] = struct.unpack_from("<I", data, 40)[0]
    ccid["dwMaxCCIDMessageLength"] = struct.unpack_from("<I", data, 44)[0]
    ccid["bClassGetResponse"] = data[48]
    ccid["bClassEnvelope"] = data[49]
    ccid["wLcdLayout"] = struct.unpack_from("<H", data, 50)[0]
    ccid["bPINSupport"] = data[52]
    ccid["bMaxCCIDBusySlots"] = data[53]

    return ccid


def validate_descriptor_against_spec(label, reader_info, conn):
    """Firmware-only: validate USB descriptor matches Cherry profile spec."""
    if label != "firmware":
        return {"note": "Descriptor validation only for Cherry firmware reader"}

    desc = get_usb_descriptor()
    errors = []

    if desc.get("bInterfaceClass") != 0xFF:
        errors.append(f"bInterfaceClass: expected 0xFF (vendor-specific), got {desc.get('bInterfaceClass')}")
    if desc.get("bInterfaceProtocol") != 0x00:
        errors.append(f"bInterfaceProtocol: expected 0x00 (CCID), got {desc.get('bInterfaceProtocol')}")
    if desc.get("bNumEndpoints") != 3:
        errors.append(f"bNumEndpoints: expected 3, got {desc.get('bNumEndpoints')}")
    if "Cherry" not in desc.get("iManufacturer", ""):
        errors.append(f"iManufacturer: expected 'Cherry', got '{desc.get('iManufacturer')}'")
    if "ST-2xxx" not in desc.get("iProduct", ""):
        errors.append(f"iProduct: expected 'ST-2xxx', got '{desc.get('iProduct')}'")

    ccid = desc.get("ccid", {})
    if "error" in ccid:
        errors.append(f"CCID parse error: {ccid['error']}")
    else:
        for field, expected in CHERRY_EXPECTED.items():
            actual = ccid.get(field)
            if field == "bcdCCID":
                if actual != "1.00":
                    errors.append(f"CCID.{field}: expected '1.00', got '{actual}'")
            elif isinstance(expected, int):
                if actual != expected:
                    if expected > 0xFFFF:
                        errors.append(
                            f"CCID.{field}: expected 0x{expected:08X}, got 0x{actual:08X}"
                        )
                    else:
                        errors.append(
                            f"CCID.{field}: expected 0x{expected:04X}, got 0x{actual:04X}"
                        )

    if errors:
        raise AssertionError("Descriptor mismatches:\n  " + "\n  ".join(errors))

    return {
        "vid": f"0x{desc['vid']:04X}",
        "pid": f"0x{desc['pid']:04X}",
        "interface_class": f"0x{desc.get('bInterfaceClass', 0):02X}",
        "manufacturer": desc.get("iManufacturer", ""),
        "product": desc.get("iProduct", ""),
        "serial": desc.get("iSerial", ""),
        "bcdCCID": ccid.get("bcdCCID"),
        "dwFeatures": f"0x{ccid.get('dwFeatures', 0):08X}",
        "bPINSupport": f"0x{ccid.get('bPINSupport', 0):02X}",
        "bClassGetResponse": f"0x{ccid.get('bClassGetResponse', 0):02X}",
        "dwMaxCCIDMessageLength": ccid.get("dwMaxCCIDMessageLength"),
        "dwDefaultClock": ccid.get("dwDefaultClock"),
        "voltageSupport": f"0x{ccid.get('bVoltageSupport', 0):02X}",
    }


def test_pinpad_descriptor(label, reader_info, conn):
    """Firmware-only: verify PIN pad capability bits in descriptor."""
    if label != "firmware":
        return {"note": "PIN pad check only for Cherry firmware reader"}

    desc = get_usb_descriptor()
    ccid = desc.get("ccid", {})
    if "error" in ccid:
        raise AssertionError(f"CCID parse error: {ccid['error']}")

    pin_support = ccid.get("bPINSupport", 0)
    errors = []
    if not (pin_support & 0x01):
        errors.append("bPINSupport bit 0 (verify) not set")
    if not (pin_support & 0x02):
        errors.append("bPINSupport bit 1 (modify) not set")

    # Note: real Cherry ST-2xxx does NOT have dwFeatures bit 18 set,
    # even though bPINSupport = 0x03. PIN is indicated by bPINSupport only.
    # We check bPINSupport, not dwFeatures bit 18.

    if errors:
        raise AssertionError("PIN pad capability issues:\n  " + "\n  ".join(errors))

    return {
        "bPINSupport": f"0x{pin_support:02X}",
        "verify": bool(pin_support & 0x01),
        "modify": bool(pin_support & 0x02),
    }


def test_escape_rejection(label, reader_info, conn):
    """Firmware-only: verify ESCAPE returns CMD_NOT_SUPPORTED for Cherry."""
    if label != "firmware":
        return {"note": "ESCAPE rejection only applicable to Cherry firmware reader"}

    if not LOG_FILE.exists():
        return {"note": "No CCID log available"}

    msgs = parse_ccid_log(str(LOG_FILE), reader_info["serial"])

    escape_responses = [m for m in msgs if m.msg_type == 0x64 and m.direction == "IN"]

    if not escape_responses:
        return {"note": "No ESCAPE responses in CCID log (pcscd may not have sent ESCAPE)"}

    for resp in escape_responses:
        if resp.b_error != 0x0A:
            raise AssertionError(
                f"ESCAPE bError: expected CMD_NOT_SUPPORTED (0x0A), "
                f"got 0x{resp.b_error:02X} ({resp.error_name})"
            )

    return {
        "escape_responses": len(escape_responses),
        "all_rejected": all(r.b_error == 0x0A for r in escape_responses),
    }


def test_cherry_power_cycle(label, reader_info, conn):
    if conn is not None:
        disconnect_reader(conn)
    time.sleep(0.5)
    conn = connect_reader(reader_info)
    atr1 = get_atr(conn)
    disconnect_reader(conn)
    time.sleep(0.5)
    conn = connect_reader(reader_info)
    atr2 = get_atr(conn)
    disconnect_reader(conn)
    return {"atr1": atr1.hex(), "atr2": atr2.hex(), "match": atr1 == atr2}


def test_identical_atr(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    atr = get_atr(conn)
    disconnect_reader(conn)
    return {"atr": atr.hex(), "atr_len": len(atr)}


def test_identical_select_mf(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00A40400023F00"))
    disconnect_reader(conn)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_select_piv(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00A4040008A00000030800001000"))
    disconnect_reader(conn)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_select_openpgp(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00A4040006D276000124010200"))
    disconnect_reader(conn)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_get_response(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00C0000000"))
    disconnect_reader(conn)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_envelope(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00C2000010") + b"\x00" * 16)
    disconnect_reader(conn)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_select_satochip(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00A40400085F7361746F4368697000"))
    disconnect_reader(conn)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_rapid_select(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    results = []
    for apdu_hex in ["00A40400023F00"] * 20:
        data, sw = transmit_apdu(conn, bytes.fromhex(apdu_hex))
        results.append(f"0x{sw:04X}")
    disconnect_reader(conn)
    return {"sw_sequence": results, "all_same": len(set(results)) == 1}


def main():
    print("=" * 60)
    print("SOAK TEST 11: Cherry ST-2xxx Profile Specific Tests")
    print("=" * 60)

    SUITE_DIR.mkdir(parents=True, exist_ok=True)
    suite = SuiteResult(suite_name="soak-11-cherry", suite_num=11)

    print("\n[1/5] Starting pcscd with CCID capture (COMM level)...")
    pcscd_proc = restart_pcscd(debug_level=0x0007, log_path=str(LOG_FILE))
    time.sleep(3)

    print("[2/5] Discovering readers (timeout 45s)...")
    readers = discover_readers(timeout=45, firmware_serial=CHERRY_SERIAL)
    if "gemalto" not in readers:
        print("ERROR: Gemalto reader not found!", file=sys.stderr)
        stop_pcscd()
        return 1
    if "firmware" not in readers:
        print(f"ERROR: Cherry firmware reader (serial={CHERRY_SERIAL}) not found!", file=sys.stderr)
        stop_pcscd()
        return 1
    print(f"  Gemalto:  {readers['gemalto']['name']}")
    print(f"  Firmware: {readers['firmware']['name']}")

    print("[3/5] Connecting...")
    gemalto_conn = connect_reader(readers["gemalto"])
    firmware_conn = connect_reader(readers["firmware"])

    print("[4/5] Running tests...")

    print("\n  --- Firmware Self-Validation (no cross-reader) ---")
    run_test_on_both(suite, "descriptor_matches_cherry_spec",
                     validate_descriptor_against_spec, readers, gemalto_conn, firmware_conn,
                     expect_response_diff=True)
    run_test_on_both(suite, "pinpad_capability",
                     test_pinpad_descriptor, readers, gemalto_conn, firmware_conn,
                     expect_response_diff=True)
    run_test_on_both(suite, "escape_rejected",
                     test_escape_rejection, readers, gemalto_conn, firmware_conn,
                     expect_response_diff=True)

    print("\n  --- APDU Cross-Reader (same JR3180 card) ---")
    run_test_on_both(suite, "identical_atr", test_identical_atr, readers)
    run_test_on_both(suite, "identical_select_mf", test_identical_select_mf, readers)
    run_test_on_both(suite, "identical_select_piv", test_identical_select_piv, readers)
    run_test_on_both(suite, "identical_select_openpgp", test_identical_select_openpgp, readers)
    run_test_on_both(suite, "identical_select_satochip", test_identical_select_satochip, readers)
    run_test_on_both(suite, "identical_get_response", test_identical_get_response, readers)
    run_test_on_both(suite, "identical_envelope", test_identical_envelope, readers)
    run_test_on_both(suite, "cherry_power_cycle", test_cherry_power_cycle, readers)
    run_test_on_both(suite, "rapid_select_20x", test_identical_rapid_select, readers)

    print("[5/5] Saving results...")
    disconnect_reader(gemalto_conn)
    disconnect_reader(firmware_conn)
    save_results(SUITE_DIR, suite)
    stop_pcscd()

    print(f"\n{'=' * 60}")
    print(f"Results: {suite.passed}/{suite.total_tests} passed, {suite.failed} failed")
    print(f"Bugs found: {suite.bugs_found}")
    print(f"Logs: {SUITE_DIR}")
    print(f"{'=' * 60}")

    return 0 if suite.failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
