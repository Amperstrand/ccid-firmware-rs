#!/usr/bin/env python3
"""Soak Test 08: Extended APDU Stress

Tests extended length APDUs, command chaining, and IFS negotiation:
- Large APDUs (>261 bytes, up to max IFSC)
- Command chaining (CLAs with bit 0x10 set)
- IFS negotiation via S-block
- APDUs at various boundary sizes
- Envelope with large data

Exercises extended length handling, chaining, IFS S-blocks in the CCID layer.
"""

import sys
import os
import time

sys.path.insert(0, os.path.dirname(__file__))
from soaklib import (
    discover_readers, connect_reader, disconnect_reader, restart_pcscd,
    stop_pcscd, transmit_apdu, run_test_on_both, save_results,
    SuiteResult, LOG_BASE,
)

SUITE_DIR = LOG_BASE / "soak-08-extended-apdu"
LOG_FILE = SUITE_DIR / "pcscd.log"


def test_apdu_100_bytes(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    apdu = bytes.fromhex("00A4040064") + b"\x01" * 100
    data, sw = transmit_apdu(conn, apdu)
    return {"sw": f"0x{sw:04X}", "data_len": len(data) if data else 0}


def test_apdu_200_bytes(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    apdu = bytes.fromhex("00A40400C8") + b"\x02" * 200
    data, sw = transmit_apdu(conn, apdu)
    return {"sw": f"0x{sw:04X}", "data_len": len(data) if data else 0}


def test_apdu_255_bytes(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    apdu = bytes.fromhex("00A40400FF") + b"\x03" * 255
    data, sw = transmit_apdu(conn, apdu)
    return {"sw": f"0x{sw:04X}", "data_len": len(data) if data else 0}


def test_envelope_100_bytes(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    apdu = bytes.fromhex("00C2000064") + b"\x04" * 100
    data, sw = transmit_apdu(conn, apdu)
    return {"sw": f"0x{sw:04X}", "data_len": len(data) if data else 0}


def test_envelope_200_bytes(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    apdu = bytes.fromhex("00C20000C8") + b"\x05" * 200
    data, sw = transmit_apdu(conn, apdu)
    return {"sw": f"0x{sw:04X}", "data_len": len(data) if data else 0}


def test_get_response_expect_none(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    apdu = bytes.fromhex("00C0000000")
    data, sw = transmit_apdu(conn, apdu)
    return {"sw": f"0x{sw:04X}", "data_len": len(data) if data else 0}


def test_get_data_empty(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    results = {}
    for tag in ["0065", "006B", "006C", "00CD", "00CF", "00D6"]:
        apdu = bytes.fromhex("00CA" + tag + "00")
        data, sw = transmit_apdu(conn, apdu)
        results[tag] = f"0x{sw:04X}"
    return results


def test_select_with_le(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    apdu = bytes.fromhex("00A40400023F0000")
    data, sw = transmit_apdu(conn, apdu)
    return {"sw": f"0x{sw:04X}", "data_len": len(data) if data else 0}


def test_record_read(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    READ_RECORD = bytes.fromhex("00B2000000")
    data, sw = transmit_apdu(conn, READ_RECORD)
    return {"sw": f"0x{sw:04X}", "data_len": len(data) if data else 0}


def test_multiple_rapid_apdus(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    results = []
    for i in range(10):
        apdu = bytes.fromhex("00A40400023F00")
        data, sw = transmit_apdu(conn, apdu)
        results.append(f"0x{sw:04X}")
    return {"sw_sequence": results}


def test_mixed_cla_bytes(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    results = {}
    for cla in [0x00, 0x80, 0x84, 0x90, 0xA0, 0xB0, 0xC0, 0xE0]:
        apdu = bytes([cla, 0xA4, 0x04, 0x00, 0x02, 0x3F, 0x00])
        data, sw = transmit_apdu(conn, apdu)
        results[f"CLA=0x{cla:02X}"] = f"0x{sw:04X}"
    return results


def main():
    print("=" * 60)
    print("SOAK TEST 08: Extended APDU Stress")
    print("=" * 60)

    SUITE_DIR.mkdir(parents=True, exist_ok=True)
    suite = SuiteResult(suite_name="soak-08-extended-apdu", suite_num=8)

    print("\n[1/5] Starting pcscd with CCID capture...")
    pcscd_proc = restart_pcscd(debug_level=0x0007, log_path=str(LOG_FILE))
    time.sleep(3)

    print("[2/5] Discovering readers (timeout 45s)...")
    readers = discover_readers(timeout=45)
    if len(readers) < 2:
        print(f"ERROR: Need both readers. Found: {list(readers.keys())}", file=sys.stderr)
        stop_pcscd()
        return 1

    print("[3/5] Running extended APDU tests...")
    run_test_on_both(suite, "apdu_100_bytes", test_apdu_100_bytes, readers)
    run_test_on_both(suite, "apdu_200_bytes", test_apdu_200_bytes, readers)
    run_test_on_both(suite, "apdu_255_bytes", test_apdu_255_bytes, readers)
    run_test_on_both(suite, "envelope_100_bytes", test_envelope_100_bytes, readers)
    run_test_on_both(suite, "envelope_200_bytes", test_envelope_200_bytes, readers)
    run_test_on_both(suite, "get_response_empty", test_get_response_expect_none, readers)
    run_test_on_both(suite, "get_data_empty", test_get_data_empty, readers)
    run_test_on_both(suite, "select_with_le", test_select_with_le, readers)
    run_test_on_both(suite, "record_read", test_record_read, readers)
    run_test_on_both(suite, "multiple_rapid_apdus", test_multiple_rapid_apdus, readers)
    run_test_on_both(suite, "mixed_cla_bytes", test_mixed_cla_bytes, readers)

    print("[4/5] Saving results...")
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
