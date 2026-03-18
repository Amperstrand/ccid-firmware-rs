#!/usr/bin/env python3
"""Soak Test 05: PIV Operations

Tests NIST SP 800-73-4 PIV card interface:
- Select PIV AID
- Discover PIV objects
- Read card capabilities
- Verify PIN (if available)
- Test PIV-specific APDUs

PIV applet should be installed via soak-02 (GlobalPlatform) first.
"""

import sys
import os
import subprocess
import time

sys.path.insert(0, os.path.dirname(__file__))
from soaklib import (
    discover_readers, connect_reader, disconnect_reader, restart_pcscd,
    stop_pcscd, transmit_apdu, run_test_on_both, save_results,
    SuiteResult, LOG_BASE,
)

SUITE_DIR = LOG_BASE / "soak-05-piv"
LOG_FILE = SUITE_DIR / "pcscd.log"

PIV_AID = bytes.fromhex("A00000030800001000")
PIV_AID_V2 = bytes.fromhex("A000000308000010001000")


def test_select_piv_aid(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    SELECT = bytes.fromhex("00A4040008") + PIV_AID
    data, sw = transmit_apdu(conn, SELECT)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_select_piv_aid_v2(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    SELECT = bytes.fromhex("00A404000C") + PIV_AID_V2
    data, sw = transmit_apdu(conn, SELECT)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_piv_get_chuid(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    GET_CHUID = bytes.fromhex("00CB3FFF0500001000002D0035000800002D010C00000000000000000000000000000000")
    data, sw = transmit_apdu(conn, GET_CHUID)
    return {"sw": f"0x{sw:04X}", "data_len": len(data) if data else 0, "data_prefix": data.hex()[:100] if data else ""}


def test_piv_get_ccc(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    GET_CCC = bytes.fromhex("00CB3FFF0500001000DB0008")
    data, sw = transmit_apdu(conn, GET_CCC)
    return {"sw": f"0x{sw:04X}", "data_len": len(data) if data else 0}


def test_piv_discovery(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    YUBIPIV_DISCOVERY = bytes.fromhex("00FB00000000")
    data, sw = transmit_apdu(conn, YUBIPIV_DISCOVERY)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_piv_verify_pin(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    VERIFY_PIN = bytes.fromhex("0020008008") + b"12345678" + b"00"
    data, sw = transmit_apdu(conn, VERIFY_PIN)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_piv_get_serial(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    GET_SERIAL = bytes.fromhex("00CB3FFF05000C0002FC100000000000")
    data, sw = transmit_apdu(conn, GET_SERIAL)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_piv_pin_policy(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    PIN_POLICY = bytes.fromhex("00CB3FFF050000000000000200000000")
    data, sw = transmit_apdu(conn, PIN_POLICY)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_piv_key_history(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    KEY_HISTORY = bytes.fromhex("00CB3FFF050000100000100000020000000000000000000000")
    data, sw = transmit_apdu(conn, KEY_HISTORY)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_piv_yubikey_auth(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    SELECT_AUTH = bytes.fromhex("00A4040005A0000003")
    data, sw = transmit_apdu(conn, SELECT_AUTH)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_opensc_piv_tool(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    cmd = f"pkcs15-tool --read-certificate 2 2>&1"
    try:
        result = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=30, env={
            **os.environ, "OPENSC_CONF": "/etc/opensc.conf"
        })
        return {"stdout": result.stdout[:500], "rc": result.returncode}
    except subprocess.TimeoutExpired:
        return {"stdout": "", "rc": -1, "error": "TIMEOUT"}


def main():
    import subprocess
    print("=" * 60)
    print("SOAK TEST 05: PIV Operations")
    print("=" * 60)

    SUITE_DIR.mkdir(parents=True, exist_ok=True)
    suite = SuiteResult(suite_name="soak-05-piv", suite_num=5)

    print("\n[1/5] Starting pcscd with CCID capture...")
    pcscd_proc = restart_pcscd(debug_level=0x0007, log_path=str(LOG_FILE))
    time.sleep(3)

    print("[2/5] Discovering readers (timeout 45s)...")
    readers = discover_readers(timeout=45)
    if len(readers) < 2:
        print(f"ERROR: Need both readers. Found: {list(readers.keys())}", file=sys.stderr)
        stop_pcscd()
        return 1

    print("[3/5] Running PIV tests...")
    run_test_on_both(suite, "select_piv_aid", test_select_piv_aid, readers)
    run_test_on_both(suite, "select_piv_aid_v2", test_select_piv_aid_v2, readers)
    run_test_on_both(suite, "piv_get_chuid", test_piv_get_chuid, readers)
    run_test_on_both(suite, "piv_get_ccc", test_piv_get_ccc, readers)
    run_test_on_both(suite, "piv_discovery", test_piv_discovery, readers)
    run_test_on_both(suite, "piv_verify_pin", test_piv_verify_pin, readers)
    run_test_on_both(suite, "piv_get_serial", test_piv_get_serial, readers)
    run_test_on_both(suite, "piv_pin_policy", test_piv_pin_policy, readers)
    run_test_on_both(suite, "piv_key_history", test_piv_key_history, readers)
    run_test_on_both(suite, "piv_yubikey_auth", test_piv_yubikey_auth, readers)

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
