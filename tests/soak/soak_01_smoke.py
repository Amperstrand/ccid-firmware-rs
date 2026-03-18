#!/usr/bin/env python3
"""Soak Test 01: Smoke Test

Basic CCID operations on both readers:
- Reader discovery
- Connect / disconnect
- ATR retrieval
- GET_SLOT_STATUS
- POWER_ON / POWER_OFF cycle
- Basic SELECT APDU
"""

import sys
import os
import time

sys.path.insert(0, os.path.dirname(__file__))
from soaklib import (
    discover_readers, connect_reader, disconnect_reader, restart_pcscd,
    stop_pcscd, transmit_apdu, run_test_on_both, save_results,
    SuiteResult, LOG_BASE, GEMALTO_SERIAL, FIRMWARE_SERIAL, get_atr,
)

SUITE_DIR = LOG_BASE / "soak-01-smoke"
LOG_FILE = SUITE_DIR / "pcscd.log"

SELECT_AID_MF = bytes.fromhex("00A4040000")


def test_connect_atr(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    atr = get_atr(conn)
    conn.disconnect()
    return {"atr": atr.hex(), "atr_len": len(atr)}


def test_select_mf(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    data, sw = transmit_apdu(conn, SELECT_AID_MF)
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_power_cycle(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    conn.disconnect()
    time.sleep(0.5)
    conn = connect_reader(reader_info)
    atr2 = get_atr(conn)
    conn.disconnect()
    return {"atr_after_cycle": atr2.hex(), "atr_len": len(atr2)}


def test_double_connect(label, reader_info, conn):
    if conn is not None:
        disconnect_reader(conn)
    conn1 = connect_reader(reader_info)
    atr1 = get_atr(conn1)
    disconnect_reader(conn1)
    time.sleep(0.3)
    conn2 = connect_reader(reader_info)
    atr2 = get_atr(conn2)
    disconnect_reader(conn2)
    return {"atr1": atr1.hex(), "atr2": atr2.hex(), "match": atr1 == atr2}


def test_get_response_empty(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    GET_RESPONSE = bytes.fromhex("00C0000000")
    data, sw = transmit_apdu(conn, GET_RESPONSE)
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_envelope_empty(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    ENVELOPE = bytes.fromhex("00C2000000")
    data, sw = transmit_apdu(conn, ENVELOPE)
    conn.disconnect()
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def main():
    print("=" * 60)
    print("SOAK TEST 01: Smoke Test")
    print("=" * 60)

    SUITE_DIR.mkdir(parents=True, exist_ok=True)
    suite = SuiteResult(suite_name="soak-01-smoke", suite_num=1)

    print("\n[1/6] Starting pcscd with CCID capture...")
    pcscd_proc = restart_pcscd(debug_level=0x0007, log_path=str(LOG_FILE))
    time.sleep(3)

    print("[2/6] Discovering readers (timeout 45s)...")
    readers = discover_readers(timeout=45)
    if "gemalto" not in readers:
        print("ERROR: Gemalto reader not found!", file=sys.stderr)
        stop_pcscd()
        return 1
    if "firmware" not in readers:
        print("ERROR: Firmware reader not found!", file=sys.stderr)
        stop_pcscd()
        return 1
    print(f"  Found: {readers['gemalto']['name']}")
    print(f"  Found: {readers['firmware']['name']}")

    print("[3/6] Connecting to both readers...")
    gemalto_conn = connect_reader(readers["gemalto"])
    firmware_conn = connect_reader(readers["firmware"])
    print(f"  Gemalto ATR: {get_atr(gemalto_conn).hex()}")
    print(f"  Firmware ATR: {get_atr(firmware_conn).hex()}")

    print("[4/6] Running smoke tests...")
    run_test_on_both(suite, "connect_and_atr", test_connect_atr, readers, gemalto_conn, firmware_conn)
    run_test_on_both(suite, "select_mf", test_select_mf, readers)
    run_test_on_both(suite, "power_cycle", test_power_cycle, readers)
    run_test_on_both(suite, "double_connect", test_double_connect, readers)
    run_test_on_both(suite, "get_response_empty", test_get_response_empty, readers)
    run_test_on_both(suite, "envelope_empty", test_envelope_empty, readers)

    print("[5/6] Disconnecting...")
    disconnect_reader(gemalto_conn)
    disconnect_reader(firmware_conn)
    time.sleep(1)

    print("[6/6] Saving results...")
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
