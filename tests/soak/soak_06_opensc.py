#!/usr/bin/env python3
"""Soak Test 06: OpenSC Toolchain

Tests the full OpenSC stack: opensc-tool, pkcs15-tool, pkcs11-tool.
This exercises the pcscd -> libccid -> CCID path through multiple host tools.
"""

import sys
import os
import subprocess
import time

sys.path.insert(0, os.path.dirname(__file__))
from soaklib import (
    discover_readers, connect_reader, disconnect_reader, restart_pcscd,
    stop_pcscd, run_test_on_both, save_results,
    SuiteResult, LOG_BASE,
)

SUITE_DIR = LOG_BASE / "soak-06-opensc-toolchain"
LOG_FILE = SUITE_DIR / "pcscd.log"


def _run_opensc(cmd: str, timeout: int = 30) -> dict:
    try:
        result = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=timeout)
        return {"rc": result.returncode, "stdout": result.stdout[:2000], "stderr": result.stderr[:500]}
    except subprocess.TimeoutExpired:
        return {"rc": -1, "stdout": "", "stderr": "TIMEOUT"}


def test_opensc_tool_atr(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _run_opensc("opensc-tool --atr 2>&1")


def test_opensc_tool_info(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _run_opensc("opensc-tool --info 2>&1")


def test_opensc_tool_list_readers(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _run_opensc("opensc-tool --list-readers 2>&1")


def test_opensc_tool_list_files(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _run_opensc("opensc-tool --list-files 2>&1")


def test_pkcs15_tool_list(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _run_opensc("pkcs15-tool --list-info 2>&1")


def test_pkcs15_tool_read_cert(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _run_opensc("pkcs15-tool --read-certificate 2 2>&1")


def test_pkcs15_tool_list_pins(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _run_opensc("pkcs15-tool --list-pins 2>&1")


def test_pkcs15_tool_list_pk(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _run_opensc("pkcs15-tool --list-public-keys 2>&1")


def test_pkcs11_tool_test(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _run_opensc("pkcs11-tool --test --login 2>&1", timeout=60)


def test_pkcs11_tool_list_slots(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _run_opensc("pkcs11-tool --list-slots 2>&1")


def test_opensc_tool_send_apdu(label, reader_info, conn):
    if conn:
        disconnect_reader(conn)
    return _run_opensc("opensc-tool --send-apdu 00A40400023F00 2>&1")


def main():
    print("=" * 60)
    print("SOAK TEST 06: OpenSC Toolchain")
    print("=" * 60)

    SUITE_DIR.mkdir(parents=True, exist_ok=True)
    suite = SuiteResult(suite_name="soak-06-opensc-toolchain", suite_num=6)

    print("\n[1/5] Starting pcscd with CCID capture...")
    pcscd_proc = restart_pcscd(debug_level=0x0007, log_path=str(LOG_FILE))
    time.sleep(3)

    print("[2/5] Discovering readers (timeout 45s)...")
    readers = discover_readers(timeout=45)
    if len(readers) < 2:
        print(f"ERROR: Need both readers. Found: {list(readers.keys())}", file=sys.stderr)
        stop_pcscd()
        return 1

    print("[3/5] Running OpenSC toolchain tests...")
    run_test_on_both(suite, "opensc_tool_atr", test_opensc_tool_atr, readers)
    run_test_on_both(suite, "opensc_tool_info", test_opensc_tool_info, readers)
    run_test_on_both(suite, "opensc_tool_list_readers", test_opensc_tool_list_readers, readers)
    run_test_on_both(suite, "opensc_tool_list_files", test_opensc_tool_list_files, readers)
    run_test_on_both(suite, "pkcs15_tool_list", test_pkcs15_tool_list, readers)
    run_test_on_both(suite, "pkcs15_tool_read_cert", test_pkcs15_tool_read_cert, readers)
    run_test_on_both(suite, "pkcs15_tool_list_pins", test_pkcs15_tool_list_pins, readers)
    run_test_on_both(suite, "pkcs15_tool_list_pk", test_pkcs15_tool_list_pk, readers)
    run_test_on_both(suite, "pkcs11_tool_list_slots", test_pkcs11_tool_list_slots, readers)
    run_test_on_both(suite, "opensc_tool_send_apdu", test_opensc_tool_send_apdu, readers)

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
