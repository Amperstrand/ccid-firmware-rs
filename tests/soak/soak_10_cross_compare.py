#!/usr/bin/env python3
"""Soak Test 10: Cross-Reader Comparison

Runs identical operations on both Gemalto and firmware readers,
comparing APDU responses and CCID behavior.

This is the definitive behavioral comparison test.
"""

import sys
import os
import time
import json

sys.path.insert(0, os.path.dirname(__file__))
from soaklib import (
    discover_readers, connect_reader, disconnect_reader, restart_pcscd,
    stop_pcscd, transmit_apdu, run_test_on_both, save_results,
    SuiteResult, LOG_BASE,
)

SUITE_DIR = LOG_BASE / "soak-10-cross-compare"
LOG_FILE = SUITE_DIR / "pcscd.log"


def test_identical_atr(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    atr = conn.getATR()
    if isinstance(atr, list):
        atr = bytes(atr)
    conn.disconnect()
    return {"atr": atr.hex(), "atr_len": len(atr)}


def test_identical_select_mf(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00A40400023F00"))
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_select_piv(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00A4040008A00000030800001000"))
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_select_openpgp(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00A4040006D276000124010200"))
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_envelope(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00C2000010") + b"\x00" * 16)
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_get_response(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00C0000000"))
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_get_data_65(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00CA006500"))
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex()[:100] if data else ""}


def test_identical_get_data_6b(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00CA006B00"))
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex()[:100] if data else ""}


def test_identical_select_satochip(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00A40400085F7361746F4368697000"))
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_record_read(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00B2000000"))
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_read_binary(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00B0000004"))
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_pw_status(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, bytes.fromhex("00CA00C400"))
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_identical_multi_select(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    results = []
    for apdu_hex in ["00A40400023F00", "00A4040008A00000030800001000", "00A4040006D276000124010200", "00A40400023F00"]:
        data, sw = transmit_apdu(conn, bytes.fromhex(apdu_hex))
        results.append(f"0x{sw:04X}")
    conn.disconnect()
    return {"sw_sequence": results}


def test_identical_rapid_apdus(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    results = []
    for _ in range(20):
        data, sw = transmit_apdu(conn, bytes.fromhex("00A40400023F00"))
        results.append(f"0x{sw:04X}")
    conn.disconnect()
    return {"sw_sequence": results, "all_same": len(set(results)) == 1}


def main():
    print("=" * 60)
    print("SOAK TEST 10: Cross-Reader Comparison")
    print("=" * 60)

    SUITE_DIR.mkdir(parents=True, exist_ok=True)
    suite = SuiteResult(suite_name="soak-10-cross-compare", suite_num=10)

    print("\n[1/5] Starting pcscd with CCID capture...")
    pcscd_proc = restart_pcscd(debug_level=0x0003, log_path=str(LOG_FILE))
    time.sleep(3)

    print("[2/5] Discovering readers (timeout 45s)...")
    readers = discover_readers(timeout=45)
    if len(readers) < 2:
        print(f"ERROR: Need both readers. Found: {list(readers.keys())}", file=sys.stderr)
        stop_pcscd()
        return 1

    print("[3/5] Running cross-reader comparison tests...")
    run_test_on_both(suite, "identical_atr", test_identical_atr, readers)
    run_test_on_both(suite, "identical_select_mf", test_identical_select_mf, readers)
    run_test_on_both(suite, "identical_select_piv", test_identical_select_piv, readers)
    run_test_on_both(suite, "identical_select_openpgp", test_identical_select_openpgp, readers)
    run_test_on_both(suite, "identical_select_satochip", test_identical_select_satochip, readers)
    run_test_on_both(suite, "identical_envelope", test_identical_envelope, readers)
    run_test_on_both(suite, "identical_get_response", test_identical_get_response, readers)
    run_test_on_both(suite, "identical_get_data_65", test_identical_get_data_65, readers)
    run_test_on_both(suite, "identical_get_data_6b", test_identical_get_data_6b, readers)
    run_test_on_both(suite, "identical_pw_status", test_identical_pw_status, readers)
    run_test_on_both(suite, "identical_record_read", test_identical_record_read, readers)
    run_test_on_both(suite, "identical_read_binary", test_identical_read_binary, readers)
    run_test_on_both(suite, "identical_multi_select", test_identical_multi_select, readers)
    run_test_on_both(suite, "identical_rapid_apdus_20x", test_identical_rapid_apdus, readers)

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
