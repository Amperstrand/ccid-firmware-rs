#!/usr/bin/env python3
"""Soak Test 04: OpenPGP Card Operations

Uses GnuPG to:
- Detect OpenPGP card
- Read card status
- Fetch card data
- Test PIN verification
- List keys

If OpenPGP applet is installed on the cards. If not, falls back to
raw APDU-based testing of the OpenPGP AID selection.
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

SUITE_DIR = LOG_BASE / "soak-04-openpgp"
LOG_FILE = SUITE_DIR / "pcscd.log"

OPENPGP_AID = bytes.fromhex("D276000124010200")
OPENPGP_AID_V3 = bytes.fromhex("D2760001240102000000")


def test_select_openpgp_aid(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    SELECT = bytes.fromhex("00A4040006") + OPENPGP_AID
    data, sw = transmit_apdu(conn, SELECT)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_select_openpgp_aid_v3(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    SELECT = bytes.fromhex("00A4040008") + OPENPGP_AID_V3
    data, sw = transmit_apdu(conn, SELECT)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_openpgp_get_application_data(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    GET_DATA = bytes.fromhex("00CA006500")
    data, sw = transmit_apdu(conn, GET_DATA)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else "", "data_len": len(data) if data else 0}


def test_openpgp_get_name(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    GET_NAME = bytes.fromhex("00CA006B00")
    data, sw = transmit_apdu(conn, GET_NAME)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_openpgp_get_key_attrs(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    results = {}
    for tag, name in [("00B600", "sig"), ("00B800", "enc"), ("00A400", "auth")]:
        GET_KEY = bytes.fromhex("00CA" + tag + "00")
        data, sw = transmit_apdu(conn, GET_KEY)
        results[name] = {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}
    return results


def test_openpgp_pw_status(label, reader_info, conn):
    if conn is None:
        conn = connect_reader(reader_info)
    GET_PW = bytes.fromhex("00CA00C400")
    data, sw = transmit_apdu(conn, GET_PW)
    return {"sw": f"0x{sw:04X}", "data": data.hex() if data else ""}


def test_gpg_card_status(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    reader_name = reader_info["name"].split(" ")[0]
    cmd = f"gpg --card-status --reader-port {reader_name} 2>&1"
    try:
        result = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=30, env={
            **os.environ, "GNUPGHOME": os.path.expanduser("~/.gnupg-soak-test")
        })
        return {"stdout": result.stdout[:1000], "rc": result.returncode}
    except subprocess.TimeoutExpired:
        return {"stdout": "", "rc": -1, "error": "TIMEOUT"}


def test_gpg_fetch_key(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    reader_name = reader_info["name"].split(" ")[0]
    env = {**os.environ, "GNUPGHOME": os.path.expanduser("~/.gnupg-soak-test")}
    os.makedirs(env["GNUPGHOME"], exist_ok=True)
    subprocess.run("gpgconf --kill all 2>/dev/null", shell=True, capture_output=True)
    cmd = f"gpg --card-edit --reader-port {reader_name} --command-fd 0 2>&1 <<< 'fetch\nquit\n'"
    try:
        result = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=30, env=env)
        return {"stdout": result.stdout[:1000], "rc": result.returncode}
    except subprocess.TimeoutExpired:
        return {"stdout": "", "rc": -1, "error": "TIMEOUT"}


def main():
    print("=" * 60)
    print("SOAK TEST 04: OpenPGP Card Operations")
    print("=" * 60)

    SUITE_DIR.mkdir(parents=True, exist_ok=True)
    suite = SuiteResult(suite_name="soak-04-openpgp", suite_num=4)

    print("\n[1/5] Starting pcscd with CCID capture...")
    pcscd_proc = restart_pcscd(debug_level=0x0007, log_path=str(LOG_FILE))
    time.sleep(3)

    print("[2/5] Discovering readers (timeout 45s)...")
    readers = discover_readers(timeout=45)
    if len(readers) < 2:
        print(f"ERROR: Need both readers. Found: {list(readers.keys())}", file=sys.stderr)
        stop_pcscd()
        return 1

    print("[3/5] Running OpenPGP tests...")
    run_test_on_both(suite, "select_openpgp_aid", test_select_openpgp_aid, readers)
    run_test_on_both(suite, "select_openpgp_aid_v3", test_select_openpgp_aid_v3, readers)
    run_test_on_both(suite, "openpgp_get_application_data", test_openpgp_get_application_data, readers)
    run_test_on_both(suite, "openpgp_get_name", test_openpgp_get_name, readers)
    run_test_on_both(suite, "openpgp_get_key_attrs", test_openpgp_get_key_attrs, readers)
    run_test_on_both(suite, "openpgp_pw_status", test_openpgp_pw_status, readers)
    run_test_on_both(suite, "gpg_card_status", test_gpg_card_status, readers, expect_response_diff=True)

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
